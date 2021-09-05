
use std::cmp::{self, Ordering};
use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Debug};
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{PathBuf, Path};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Instant;
use crossbeam;
use crossbeam::sync::SegQueue;
use itertools::{Itertools, Partition};

#[derive(Clone)]
pub struct SyncBuilder {
    parallel_copies: u8,
    copy_contents_if_date_mismatched: bool,
    copy_contents_if_size_mismatched: bool,
    // Compares the first X bytes and last X bytes of the file and copies the file if they don't
    // match. Set to zero to turn off.
    copy_contents_if_start_end_mismatched_size: u32,
    copy_contents_if_contents_mismatched: bool, // TODO: currently ignored
    copy_created_date: bool,   // TODO: currently ignored
    copy_modified_date: bool,   // TODO: currently ignored
    directories: Vec<(PathBuf, PathBuf)>,
    filter: Option<Arc<Fn(&Path) -> bool + Send + Sync>>,
}

impl SyncBuilder {
    pub fn new() -> Self {
        SyncBuilder {
            parallel_copies: 1,
            copy_contents_if_date_mismatched: false,
            copy_contents_if_size_mismatched: true,
            copy_contents_if_start_end_mismatched_size: 8 * 1024,
            copy_contents_if_contents_mismatched: false,
            copy_created_date: true,
            copy_modified_date: true,
            directories: vec![],
            filter: None,
        }
    }

    pub fn parallel_copies(&mut self, value: u8) -> &mut Self {
        self.parallel_copies = value;
        self
    }

    pub fn copy_contents_if_date_mismatched(&mut self, value: bool) -> &mut Self {
        self.copy_contents_if_date_mismatched = value;
        self
    }

    pub fn copy_contents_if_size_mismatched(&mut self, value: bool) -> &mut Self {
        self.copy_contents_if_size_mismatched = value;
        self
    }

    pub fn copy_contents_if_start_end_mismatched_size(&mut self, value: u32) -> &mut Self {
        self.copy_contents_if_start_end_mismatched_size = value;
        self
    }

    pub fn copy_contents_if_contents_mismatched(&mut self, value: bool) -> &mut Self {
        self.copy_contents_if_contents_mismatched = value;
        self
    }

    pub fn copy_created_date(&mut self, value: bool) -> &mut Self {
        self.copy_created_date = value;
        self
    }

    pub fn copy_modified_date(&mut self, value: bool) -> &mut Self {
        self.copy_modified_date = value;
        self
    }

    pub fn add_directory_pair(&mut self, src: PathBuf, dest: PathBuf) -> &mut Self {
        self.directories.push((src, dest));
        self
    }

    /// Adds a filter that will be passed the path to each file and directory in the source
    /// before it is copied. If the function returns true, then the file/directory will be synced
    /// normally. If it returns false, it will be as if the file/directory does not exist. It will
    /// not be copied and will be deleted if it exists in the destination.
    pub fn filter<F: Fn(&Path) -> bool + 'static + Send + Sync>(&mut self, f: F) -> &mut Self {
        // I'd kind of like to not have the closure be 'static, but then a lifetime parameter infects
        // SyncBuilder and SyncOperation.
        self.filter = Some(Arc::new(f));
        self
    }

    pub fn sync(&mut self) -> SyncOperation {
        let op = SyncOperation::new(&self);
        {
            let op = op.clone();
            thread::spawn(move || op.run());
        }
        op
    }
}

impl Debug for SyncBuilder {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let filter_opt = self.filter.as_ref().map(|_| "closure");
        f.debug_struct("SyncBuilder")
            .field("parallel_copies", &self.parallel_copies)
            .field("copy_contents_if_date_mismatched", &self.copy_contents_if_date_mismatched)
            .field("copy_contents_if_size_mismatched", &self.copy_contents_if_size_mismatched)
            .field("copy_contents_if_start_end_mismatched_size", &self.copy_contents_if_start_end_mismatched_size)
            .field("copy_contents_if_contents_mismatched", &self.copy_contents_if_contents_mismatched)
            .field("copy_created_date", &self.copy_created_date)
            .field("copy_modified_date", &self.copy_modified_date)
            .field("directories", &self.directories)
            .field("filter", &filter_opt)
            .finish()
    }
}

#[derive(Debug)]
pub enum SyncLogLevel {
    Info,
    Debug,
    Error,
}

#[derive(Debug)]
pub struct SyncLogEntry {
    pub time: Instant,
    pub level: SyncLogLevel,
    pub message: String,
}

#[derive(Debug)]
struct DoneData {
    waiting_count: u8,
    done: bool,
}

struct SyncOperationData {
    options: SyncBuilder,

    // It would be good to try using Mutex(s) and see if lockfree buys any performance.
    // I know it wouldn't with my primary usecase of copying across a network, but maybe
    // it does SSD to SSD.
    log_queue: SegQueue<SyncLogEntry>,
    sync_dir_queue: SegQueue<(PathBuf, PathBuf)>,
    op_queue: SegQueue<IoOperation>,

    done_data: Mutex<DoneData>,
    done_condvar: Condvar,
    // errors
}

#[derive(Clone)]
pub struct SyncOperation(Arc<SyncOperationData>);

impl SyncOperation {
    pub fn new(sync_builder: &SyncBuilder) -> Self {
        SyncOperation(Arc::new(SyncOperationData {
            options: sync_builder.clone(),
            log_queue: SegQueue::new(),
            sync_dir_queue: SegQueue::new(),
            op_queue: SegQueue::new(),
            done_data: Mutex::new(DoneData {
                waiting_count: 0,
                done: false,
            }),
            done_condvar: Condvar::new(),
        }))
    }

    pub fn is_done(&self) -> bool {
        let done_data = self.0.done_data.lock().unwrap();
        done_data.done
    }

    pub fn read_log(&self) -> Option<SyncLogEntry> {
        self.0.log_queue.try_pop()
    }

    fn run(&self) {
        for &(ref src, ref dest) in &self.0.options.directories {
            self.0.sync_dir_queue.push((src.into(), dest.into()));
        }

        // TODO: normally, I much prefer using thread pools, but you can create 10 threads in 0.3 ms,
        // so it is a drop in the bucket compared to the file operations.
        crossbeam::scope(|scope| {
            for _ in 0..self.0.options.parallel_copies {
                scope.spawn(|| {
                    self.sync_thread();
                });
            }
        });
    }

    fn sync_thread(&self) {
        loop {
            if let Some(op) = self.0.op_queue.try_pop() {
                match op {
                    IoOperation::CopyFileIfNeeded(data) => {
                        self.copy_file_if_needed(data);
                    },
                    IoOperation::DeleteDirAll(ref dir) => {
                        if let Err(err) = fs::remove_dir_all(dir) {
                            self.log(SyncLogLevel::Error,
                                     format!("Failed to delete directory {}: {}",
                                     dir.to_string_lossy(), err.description()));
                        } else {
                            self.log(SyncLogLevel::Info,
                                    format!("Deleted directory {}",
                                    dir.to_string_lossy()));
                        }
                    },
                    IoOperation::DeleteFile(ref file) => {
                        if let Err(err) = fs::remove_file(file) {
                            self.log(SyncLogLevel::Error,
                                     format!("Failed to delete file {}: {}",
                                     file.to_string_lossy(), err.description()));
                        } else {
                            self.log(SyncLogLevel::Info,
                                    format!("Deleted file {}",
                                    file.to_string_lossy()));
                        }
                    },
                }
            } else if let Some((src, dest)) = self.0.sync_dir_queue.try_pop() {
                self.sync_dir(&src, &dest);
            } else {
                let mut done_data = self.0.done_data.lock().unwrap();
                if done_data.done {
                    self.log(SyncLogLevel::Debug, "Thread exiting"); // TODO: number?
                    break;
                }
                if done_data.waiting_count == self.0.options.parallel_copies - 1 {
                    done_data.done = true;
                    self.0.done_condvar.notify_all();
                    self.log(SyncLogLevel::Debug, "Thread exiting"); // TODO: number?
                    break;
                }
                done_data.waiting_count += 1;
                let mut done_data = self.0.done_condvar.wait(done_data).unwrap();
                done_data.waiting_count -= 1;
            }
        }
    }

    fn log<S: Into<String>>(&self, level: SyncLogLevel, message: S) {
        self.0.log_queue.push(SyncLogEntry {
            time: Instant::now(),
            level: level,
            message: message.into(),
        });
    }

    fn add_to_sync_dir_queue(&self, src: PathBuf, dest: PathBuf) {
        self.0.sync_dir_queue.push((src, dest));
        self.0.done_condvar.notify_one();
    }

    fn add_to_op_queue(&self, op: IoOperation) {
        self.0.op_queue.push(op);
        self.0.done_condvar.notify_one();
    }

    fn sync_dir(&self, src_dir: &Path, dest_dir: &Path) {
        // If the directory is a file or it doesn't exist, create it.
        let dest_meta = fs::symlink_metadata(&dest_dir); // TODO: should follow symlinks?
        match dest_meta {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    fs::remove_file(&dest_dir);
                    fs::create_dir(&dest_dir);
                }
            },
            Err(err) => {
                if err.kind() == io::ErrorKind::NotFound {
                    fs::create_dir(&dest_dir);
                }
            }
        }

        // List the destination directory.
        let dest_entries = match fs::read_dir(dest_dir) {
            Ok(entries) => entries,
            Err(err) => {
                self.log(SyncLogLevel::Error,
                         format!("Failed to get the list of files in {}: {}",
                         dest_dir.to_string_lossy(), err.description()));
                return;
            },
        };
        let (mut dest_entries, read_dir_errors): (HashMap<_, _>, Vec<_>) = dest_entries
                                                                           .partition_map(|res|
            match res {
                Ok(entry) => Partition::Left((entry.path(), entry)),
                Err(err) => Partition::Right(err),
            }
        );
        for err in read_dir_errors {
            self.log(SyncLogLevel::Error,
                     format!("Failed to read the name of a file in {}: {}",
                     dest_dir.to_string_lossy(), err.description()));
        }

        // Copy the contents of the source directory to the destination directory.
        let src_entries = fs::read_dir(src_dir);
        let src_entries = src_entries.unwrap(); // TODO: log error instead
        for src_entry_result in src_entries {
            match src_entry_result {
                Ok(src_entry) => {
                    let src_path = src_entry.path();
                    // If the filter returns false, skip the file, like it doesn't exist.
                    if !self.0.options.filter.as_ref().map_or(true, |f| f(&src_path)) {
                        self.log(SyncLogLevel::Info,
                                 format!("Skipping file {}", src_path.to_string_lossy()));
                        continue;
                    }
                    let dest_path = dest_dir.join(src_entry.file_name());
                    let src_meta = match src_entry.metadata() {
                        Ok(meta) => meta,
                        Err(err) => {
                            self.log(SyncLogLevel::Error,
                                     format!("Failed to read information about {}: {}",
                                     src_path.to_string_lossy(), err.description()));
                            continue;
                        },
                    };
                    let dest_entry = dest_entries.remove(&dest_path);
                    if src_meta.is_dir() {
                        self.add_to_sync_dir_queue(src_path, dest_path);
                    } else if src_meta.is_file() {
                        let dest_meta = dest_entry.map(|entry|
                            entry.metadata()
                        );
                        let dest_meta = match dest_meta {
                            Some(Err(ref err)) => {
                                if err.kind() == io::ErrorKind::NotFound {
                                    None
                                } else {
                                    self.log(SyncLogLevel::Error,
                                             format!("Failed to read information about {}: {}",
                                             dest_path.to_string_lossy(), err.description()));
                                    continue;
                                }
                            }
                            Some(Ok(meta)) => Some(meta),
                            None => None,
                        };
                        // TODO: this can probably be simplified now or especially once symlinks are
                        // deleted
                        let should_copy = match dest_meta {
                            Some(ref dest_meta) => {
                                if dest_meta.is_dir() {
                                    self.add_to_op_queue(IoOperation::DeleteDirAll(dest_path.clone()));
                                    true
                                } else if dest_meta.is_file() {
                                    true
                                } else {
                                    self.log(SyncLogLevel::Info,
                                             format!("Skipping file due to symlink at destination: {}",
                                             src_path.to_string_lossy()));
                                    false // TODO: delete symlink?
                                }
                            },
                            None => true, // The file is not in the destination.
                        };
                        if should_copy {
                            self.add_to_op_queue(IoOperation::CopyFileIfNeeded(CopyFileIfNeededData {
                                src: src_path,
                                dest: dest_path,
                                src_meta,
                                dest_meta,
                            }));
                        }
                    }
                },
                Err(err) => {
                    self.log(SyncLogLevel::Error,
                             format!("Failed to read the name of a file in {}: {}",
                             src_dir.to_string_lossy(), err.description()));
                },
            }
        }

        // Delete anything in the destination directory that isn't in the source.
        for (dest_path, dest_entry) in dest_entries {
            let dest_meta = match dest_entry.metadata() {
                Ok(dest_meta) => dest_meta,
                Err(err) => {
                    self.log(SyncLogLevel::Error,
                             format!("Failed to read information about {}: {}",
                             dest_path.to_string_lossy(), err.description()));
                    continue;
                },
            };
            if dest_meta.is_dir() {
                self.add_to_op_queue(IoOperation::DeleteDirAll(dest_path));
            } else if dest_meta.is_file() {
                self.add_to_op_queue(IoOperation::DeleteFile(dest_path));
            }
        }
    }

    fn copy_file(&self, src_path: &Path, dest_path: &Path) {
        let mut src_file = match File::open(src_path) {
            Ok(file) => file,
            Err(err) => {
                self.log(SyncLogLevel::Error,
                         format!("Failed to open {}: {}",
                         src_path.to_string_lossy(), err.description()));
                return;
            },
        };
        let mut dest_file = match File::create(dest_path) {
            Ok(file) => file,
            Err(err) => {
                self.log(SyncLogLevel::Error,
                         format!("Failed to open {}: {}",
                         dest_path.to_string_lossy(), err.description()));
                return;
            },
        };
        self.log(SyncLogLevel::Info, format!("Starting to copy {}", src_path.to_string_lossy()));
        if let Err(err) = io::copy(&mut src_file, &mut dest_file) {
            self.log(SyncLogLevel::Error,
                     format!("Failed to copy {}: {}",
                     src_path.to_string_lossy(), err.description()));
        }
    }

    fn compare_start_end_equal(&self, data: &CopyFileIfNeededData) -> Result<bool, ()> {
        let mut src_file = match File::open(&data.src) {
            Ok(file) => file,
            Err(err) => {
                self.log(SyncLogLevel::Error,
                         format!("Failed to open {}: {}",
                         data.src.to_string_lossy(), err.description()));
                return Err(());
            },
        };
        let mut dest_file = match File::open(&data.dest) {
            Ok(file) => file,
            Err(_) => {
                return Err(());
            },
        };

        let mut compare_size = self.0.options.copy_contents_if_start_end_mismatched_size as u64;
        compare_size = cmp::min(compare_size, data.src_meta.len());
        if let Some(ref dest_meta) = data.dest_meta {
            compare_size = cmp::min(compare_size, dest_meta.len());
        }
        let compare_size = compare_size as usize;

        let mut src_buffer = Vec::new();
        src_buffer.resize(compare_size, 0);
        let mut dest_buffer = Vec::new();
        dest_buffer.resize(compare_size, 0);

        match src_file.read_exact(&mut src_buffer) {
            Ok(size) => size,
            Err(err) => {
                self.log(SyncLogLevel::Error,
                         format!("Failed to read {}: {}",
                         data.src.to_string_lossy(), err.description()));
                         return Err(());
            }
        };
        match dest_file.read_exact(&mut dest_buffer) {
            Ok(size) => size,
            Err(_) => {
                return Err(());
            }
        };

        if src_buffer != dest_buffer {
            return Ok(false);
        }

        if let Err(err) = src_file.seek(SeekFrom::End(-(compare_size as i64))) {
            self.log(SyncLogLevel::Error,
                     format!("Failed to seek {}: {}",
                     data.src.to_string_lossy(), err.description()));
            return Err(());
        }
        if let Err(_) = dest_file.seek(SeekFrom::End(-(compare_size as i64))) {
            return Err(());
        }

        match src_file.read_exact(&mut src_buffer) {
            Ok(size) => size,
            Err(err) => {
                self.log(SyncLogLevel::Error,
                         format!("Failed to seek {}: {}",
                         data.src.to_string_lossy(), err.description()));
                return Err(());
            }
        };
        match dest_file.read_exact(&mut dest_buffer) {
            Ok(size) => size,
            Err(_) => {
                return Err(());
            }
        };

        if src_buffer != dest_buffer {
            return Ok(false);
        }

        Ok(true)
    }

    fn should_copy_file(&self, data: &CopyFileIfNeededData) -> CopyReason {
        // Compare the modified date and size, depending on settings.
        let src_modified = match data.src_meta.modified() {
            Ok(modified) => modified,
            Err(err) => {
                self.log(SyncLogLevel::Error,
                         format!("Failed to get modified date of {}: {}",
                         data.src.to_string_lossy(), err.description()));
                return CopyReason::DateMismatched;
            },
        };
        let dest_meta = match data.dest_meta {
            Some(ref meta) => meta,
            None => return CopyReason::Missing,
        };
        let dest_modified = match dest_meta.modified() {
            Ok(modified) => modified,
            Err(err) => {
                self.log(SyncLogLevel::Error,
                         format!("Failed to get modified date of {}: {}",
                         data.dest.to_string_lossy(), err.description()));
                return CopyReason::DateMismatched;
            },
        };
        if self.0.options.copy_contents_if_date_mismatched &&
           src_modified != dest_modified
        {
            CopyReason::DateMismatched
        } else if self.0.options.copy_contents_if_size_mismatched &&
            data.src_meta.len() != dest_meta.len()
        {
            CopyReason::SizeMismatched
        } else if self.0.options.copy_contents_if_start_end_mismatched_size > 0 &&
            !self.compare_start_end_equal(&data).unwrap_or(false)
        {
            CopyReason::StartEndMismatched
        } else {
            CopyReason::None
        }
    }

    fn copy_file_if_needed(&self, data: CopyFileIfNeededData) {
        let copy_reason = self.should_copy_file(&data);
        if copy_reason == CopyReason::None {
            return;
        }

        let mut src_file = match File::open(&data.src) {
            Ok(file) => file,
            Err(err) => {
                self.log(SyncLogLevel::Error,
                         format!("Failed to open {}: {}",
                         data.src.to_string_lossy(), err.description()));
                return;
            },
        };
        let mut dest_file = match File::create(&data.dest) {
            Ok(file) => file,
            Err(err) => {
                self.log(SyncLogLevel::Error,
                         format!("Failed to open {}: {}",
                         data.dest.to_string_lossy(), err.description()));
                return;
            },
        };

        self.log(SyncLogLevel::Info,
            format!("{:?}: Starting to copy {}", copy_reason, data.src.to_string_lossy()));
        if let Err(err) = io::copy(&mut src_file, &mut dest_file) {
            self.log(SyncLogLevel::Error,
                     format!("Failed to copy {}: {}",
                     data.src.to_string_lossy(), err.description()));
        }
    }

}

struct CopyFileIfNeededData {
        pub src: PathBuf,
        pub dest: PathBuf,
        pub src_meta: Metadata,
        pub dest_meta: Option<Metadata>,
    }

enum IoOperation {
    DeleteDirAll(PathBuf),
    DeleteFile(PathBuf),
    CopyFileIfNeeded(CopyFileIfNeededData),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopyReason {
    Missing,
    DateMismatched,
    SizeMismatched,
    StartEndMismatched,
    None,
}


#[cfg(test)]
mod tests {
    use std::env;
    use std::fs::{self, File};
    use std::io::{self, Read, Write};
    use std::path::Path;
    use std::thread;
    use std::time::Duration;
    use super::SyncBuilder;

    fn read_file<P: AsRef<Path>>(path: P) -> Result<Vec<u8>, io::Error> {
        let mut f = File::open(path)?;
        let metadata = f.metadata()?;
        let mut data = Vec::with_capacity(metadata.len() as usize);
        f.read_to_end(&mut data)?;
        Ok(data)
    }

    fn write_file<P: AsRef<Path>>(path: P, data: &[u8]) -> io::Result<()> {
        let mut file = fs::File::create(path)?;
        file.write_all(data)?;
        Ok(())
    }

    fn list_dir<P: AsRef<Path>>(path: P) -> Result<Vec<String>, io::Error> {
        let entries = fs::read_dir(path)?;
        let mut entry_info = vec![];
        for entry in entries {
            let entry = entry?;
            let meta = entry.metadata()?;
            let data = if meta.is_file() {
                Some(read_file(entry.path())?)
            } else {
                None
            };
            entry_info.push((meta, entry.file_name(), data));
        }
        entry_info.sort_by(|&(_, ref file_name1, _), &(_, ref file_name2, _)| file_name1.cmp(file_name2));
        Ok(entry_info.iter().map(|&(ref metadata, ref file_name, ref data)| {
            format!("{}:{}:{}", if metadata.is_dir() { "D" } else { "F" }, file_name.to_string_lossy(),
                    data.as_ref().map(|d| String::from_utf8_lossy(d)).unwrap_or("".into()))
        }).collect())
    }

    #[test]
    fn test_basic_sync() {
        let temp_dir = env::temp_dir();
        let src_dir = temp_dir.join("SyncBuilderTestsSource");
        let _ = fs::remove_dir_all(&src_dir);
        fs::create_dir(&src_dir).expect("failed to create SyncBuilderTestsSource");
        let dest_dir = temp_dir.join("SyncBuilderTestsDest");
        let _ = fs::remove_dir_all(&dest_dir);
        fs::create_dir(&dest_dir).expect("failed to create SyncBuilderTestsDest");

        write_file(src_dir.join("banana.txt"), b"cd").expect("failed to create banana.txt");
        write_file(src_dir.join("cherry.txt"), b"de").expect("failed to create cherry.txt");
        write_file(src_dir.join("grape.txt"), b"hi").expect("failed to create grape.txt");
        fs::create_dir(src_dir.join("peach.txt")).expect("failed to create peach.txt");

        write_file(dest_dir.join("apple.txt"), b"bc").expect("failed to create apple.txt");
        fs::create_dir(dest_dir.join("cherry.txt")).expect("failed to create cherry.txt");
        write_file(dest_dir.join("grape.txt"), b"hij").expect("failed to create grape.txt");
        write_file(dest_dir.join("peach.txt"), b"qr").expect("failed to create peach.txt");

        let op = SyncBuilder::new().add_directory_pair(src_dir.clone(), dest_dir.clone()).sync();
        while !op.is_done() {
            thread::sleep(Duration::from_millis(100));
        }

        let dest_list = list_dir(&dest_dir).expect("failed to list dir");
        assert_eq!(dest_list, &[
            "F:banana.txt:cd",
            "F:cherry.txt:de",
            "F:grape.txt:hi",
            "D:peach.txt:",
        ]);

        let _ = fs::remove_dir_all(&src_dir).expect("failed to delete SyncBuilderTestsSource");
        let _ = fs::remove_dir_all(&dest_dir).expect("failed to delete SyncBuilderTestsDest");
    }
}
