
#![feature(question_mark)]

#[macro_use]
extern crate clear_coat;

extern crate app_dirs;
extern crate crossbeam;
extern crate itertools;
extern crate serde_json;

#[cfg(windows)]
extern crate winapi;
#[cfg(windows)]
extern crate kernel32;

use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::rc::Rc;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use clear_coat::*;
use clear_coat::common_attrs_cbs::*;
use serde_json::Value as JsonValue;
use serde_json::builder::{ArrayBuilder, ObjectBuilder};

use sync::SyncBuilder;

#[cfg_attr(windows, path = "windows_file_times.rs")]
mod file_times;
mod sync;

struct Job {
    name: String,
    parallel_copies: u8,
    copy_contents_if_date_mismatched: bool,
    copy_contents_if_size_mismatched: bool,
    copy_created_date: bool,
    copy_modified_date: bool,
    directories: Vec<(PathBuf, PathBuf)>,
    blacklist: Vec<PathBuf>,
}

impl Default for Job {
    fn default() -> Self {
        Job {
            name: "Unnamed".into(),
            parallel_copies: 2,
            copy_contents_if_date_mismatched: false,
            copy_contents_if_size_mismatched: true,
            copy_created_date: true,
            copy_modified_date: true,
            directories: vec![],
            blacklist: vec![],
        }
    }
}

#[derive(Clone)]
struct JobPageData {
    control: Vbox,
    name_text_box: Text,
    parallel_copies_text_box: Text,
}

struct MainWindowInner {
    jobs: Vec<Job>,

    dialog: Dialog,
    job_list: List,
    job_page: JobPageData,
}

impl MainWindowInner {
    fn load_jobs(&mut self) {
        let settings_dir = match app_dirs::get_data_root(app_dirs::AppDataType::UserData) {
            Ok(dir) => dir,
            Err(err) => {
                println!("failed to get directory to load jobs: {}", err);
                // TODO: should show dialog
                return;
            },
        };
        let app_settings_dir = settings_dir.join("MirrorSync");

        let file = match File::open(&app_settings_dir.join("settings.json")) {
            Ok(file) => file,
            Err(err) => {
                println!("failed to open file to load jobs: {}", err);
                // TODO: should show dialog
                return;
            }
        };
        let reader = BufReader::new(file);

        let value: JsonValue = match serde_json::from_reader(reader) {
            Ok(v) => v,
            Err(err) => {
                println!("failed to parse settings file as JSON: {}", err);
                // TODO: should show dialog
                return;
            }
        };

        let mut jobs = vec![];
        if let Some(&JsonValue::Array(ref jobs_arr)) = value.find("jobs") {
            for job_obj in jobs_arr {
                let mut job: Job = Default::default();
                if let Some(&JsonValue::String(ref name)) = job_obj.find("name") {
                    job.name = name.clone();
                }
                if let Some(parallel_copies) = job_obj.find("parallel_copies")
                                                      .and_then(|val| val.as_u64()) {
                    job.parallel_copies = parallel_copies as u8;
                }
                jobs.push(job);
            }
        }
        self.jobs = jobs;
        self.update_job_list();
        self.update_job_page();
    }

    fn save_jobs(&self) {
        // TODO: I should create a timer and just start it here. When the timer goes off,
        // it actually saves the jobs.
        let settings_dir = match app_dirs::get_data_root(app_dirs::AppDataType::UserData) {
            Ok(dir) => dir,
            Err(err) => {
                println!("failed to get directory to save jobs: {}", err);
                // TODO: should show dialog
                return;
            },
        };
        let app_settings_dir = settings_dir.join("MirrorSync");
        if let Err(err) = fs::create_dir_all(&app_settings_dir) {
            println!("failed to create directory to save jobs: {}", err);
            // TODO: should show dialog
            return;
        }

        let json = ObjectBuilder::new()
            .insert_array("jobs", |mut builder| {
                for job in self.jobs.iter() {
                    builder = builder.push_object(|job_builder| {
                        job_builder
                            .insert("name", &job.name)
                            .insert("parallel_copies", job.parallel_copies)
                    });
                }
                builder
            })
            .build();
        let mut file = match File::create(&app_settings_dir.join("settings.json")) {
            Ok(file) => file,
            Err(err) => {
                println!("failed to create file to save jobs: {}", err);
                // TODO: should show dialog
                return;
            },
        };
        let mut writer = BufWriter::new(file);

        if let Err(err) = serde_json::ser::to_writer_pretty(&mut writer, &json) {
            println!("failed to save jobs: {}", err);
            // TODO: should show dialog
            return;
        }
    }

    fn update_job_page(&self) {
        let sel_index = self.job_list.value_single();
        if let Some(sel_index) = sel_index {
            self.job_page.name_text_box.set_value(&self.jobs[sel_index].name);
            self.job_page.parallel_copies_text_box.set_value(&self.jobs[sel_index].parallel_copies.to_string());
        }
    }

    fn update_job_list(&self) {
        let sel_index = self.job_list.value_single();
        self.job_list.set_items(self.jobs.iter().map(|job| &job.name));
        self.job_list.set_value_single(sel_index);
    }

    fn add_new_job(&mut self) {
        self.jobs.push(Default::default());
        self.update_job_list();
        self.job_list.set_value_single(Some(self.jobs.len() - 1));
        self.update_job_page();
        self.save_jobs();
    }
}

const NAME_VISIBLE_COLUMNS: u32 = 15;

#[derive(Clone)]
struct MainWindow(Rc<RefCell<MainWindowInner>>);

impl MainWindow {
    pub fn new() -> Self {
        let dialog = Dialog::new();

        let job_list_label = Label::with_title("Jobs");
        let job_list = List::new();
        job_list.set_expand(Expand::Vertical);
        job_list.set_visible_columns(NAME_VISIBLE_COLUMNS);
        let add_job_button = Button::with_title("Add");
        let delete_job_button = Button::with_title("Delete");

        let job_page = Self::create_job_page();

        let sync_button = Button::with_title("Sync");

        let main_page = hbox!(
            vbox!(
                &job_list_label,
                &job_list,
                hbox!(&add_job_button, &delete_job_button),
            ),
            vbox!(
                &job_page.control,
                hbox!(fill!(),&sync_button),
            ),
        );
        main_page.set_top_level_margin_and_gap();
        dialog.append(&main_page).expect("failed to build the window");
        dialog.set_title("Mirror Sync");

        let job_list_tmp = job_list.clone();
        let job_page_tmp = job_page.clone();
        let main_window_zyg = MainWindow(Rc::new(RefCell::new(MainWindowInner {
            jobs: vec![],
            dialog: dialog,
            job_list: job_list_tmp,
            job_page: job_page_tmp,
        })));

        let main_window = main_window_zyg.clone();
        job_list.action_event().add(move |_: &ListActionArgs|
            main_window.0.borrow().update_job_page()
        );
        let main_window = main_window_zyg.clone();
        add_job_button.action_event().add(move || main_window.0.borrow_mut().add_new_job());

        let main_window = main_window_zyg.clone();
        delete_job_button.action_event().add(move || {
            let mut inner = main_window.0.borrow_mut();
            if let Some(sel_index) = inner.job_list.value_single() {
                inner.jobs.remove(sel_index);
                inner.update_job_list();
                inner.update_job_page();
                inner.save_jobs();
            }
        });

        let main_window = main_window_zyg.clone();
        job_page.name_text_box.value_changed_event().add(move || {
            let mut inner = main_window.0.borrow_mut();
            if let Some(sel_index) = inner.job_list.value_single() {
                inner.jobs[sel_index].name = inner.job_page.name_text_box.value();
                inner.update_job_list();
                inner.save_jobs();
            };
        });

        let main_window = main_window_zyg.clone();
        job_page.parallel_copies_text_box.value_changed_event().add(move || {
            let mut inner = main_window.0.borrow_mut();
            if let Some(sel_index) = inner.job_list.value_single() {
                let parallel_str = inner.job_page.parallel_copies_text_box.value();
                if let Ok(parallel_copies) = parallel_str.parse::<u8>() {
                    inner.jobs[sel_index].parallel_copies = parallel_copies;
                    inner.save_jobs();
                }
            }
        });

        main_window_zyg.0.borrow_mut().load_jobs();

        main_window_zyg
    }

    fn create_job_page() -> JobPageData {
        let name_text_box = Text::new();
        name_text_box.set_visible_columns(NAME_VISIBLE_COLUMNS);
        let parallel_copies_text_box = Text::new();

        let copy_if_size_mismatched_checkbox = Toggle::new();
        copy_if_size_mismatched_checkbox.set_title("Size mismatched");
        let copy_if_size_mismatched_indent = Label::new();
        copy_if_size_mismatched_indent.set_min_size(10, 0);

        let copy_if_modified_mismatched_checkbox = Toggle::new();
        copy_if_modified_mismatched_checkbox.set_title("Date modified mismatched");
        let copy_if_modified_mismatched_indent = Label::new();
        copy_if_modified_mismatched_indent.set_min_size(10, 0);

        let copy_created_checkbox = Toggle::new();
        copy_created_checkbox.set_title("Copy created date");

        let copy_modified_checkbox = Toggle::new();
        copy_modified_checkbox.set_title("Copy modified date");

        let folder_list = List::new();
        folder_list.set_expand(Expand::Yes);
        folder_list.set_visible_columns(20);
        folder_list.set_visible_lines(5);

        let source_dir_text_box = Text::new();
        source_dir_text_box.set_expand(Expand::Horizontal);
        let dest_dir_text_box = Text::new();
        dest_dir_text_box.set_expand(Expand::Horizontal);
        let add_dirs_button = Button::with_title("Add");
        let delete_dirs_button = Button::with_title("Delete");

        let blacklist_text_box = Text::new();
        blacklist_text_box.set_expand(Expand::Horizontal);
        let blacklist_add_button = Button::with_title("Add");
        let blacklist_delete_button = Button::with_title("Delete");

        let blacklist = List::new();
        blacklist.set_expand(Expand::Yes);
        blacklist.set_visible_columns(20);

        let dirs_grid = grid_box!(
            &Label::with_title("Source:"), &source_dir_text_box,
            &Label::with_title("Destination:"), &dest_dir_text_box,
        );
        dirs_grid.set_num_div(NumDiv::Fixed(2)).fit_all_to_children();

        let page = vbox!(
            hbox!(&Label::with_title("Name:"), &name_text_box),
            hbox!(&Label::with_title("Parallel jobs:"), &parallel_copies_text_box),
            &Label::with_title("Copy file contents if"),
            hbox!(copy_if_size_mismatched_indent, &copy_if_size_mismatched_checkbox),
            hbox!(copy_if_modified_mismatched_indent, &copy_if_modified_mismatched_checkbox),
            &copy_created_checkbox,
            &copy_modified_checkbox,
            hbox!(
                vbox!(
                    &Label::with_title("Folders"), &folder_list,
                    dirs_grid,
                    hbox!(fill!(), &add_dirs_button, &delete_dirs_button),
                ),
                vbox!(
                    &Label::with_title("Blacklist"), &blacklist,
                    hbox!(&Label::with_title("Filter:"), &blacklist_text_box),
                    hbox!(fill!(), &blacklist_add_button, &blacklist_delete_button),
                ),
            ),
        );

        JobPageData {
            name_text_box: name_text_box,
            parallel_copies_text_box: parallel_copies_text_box,
            control: page,
        }
    }

    pub fn dialog(&self) -> Dialog {
        self.0.borrow().dialog.clone()
    }

}

fn main() {
    // let op = SyncBuilder::new()
    //          .parallel_copies(1)
    //          .add_directory_pair(PathBuf::from(r"C:\Files"), PathBuf::from(r"D:\Backup"))
    //          .filter(|path| path != Path::new(r"C:\Files\Dev"))
    //          .sync();

    // let op = SyncBuilder::new()
    //          .parallel_copies(10)
    //          .add_directory_pair(PathBuf::from(r"C:\Songs"), PathBuf::from(r"\\SHINYONE\Users\Dan\Music\Songs"))
    //          .add_directory_pair(PathBuf::from(r"C:\Songs DL"), PathBuf::from(r"\\SHINYONE\Users\Dan\Music\Songs DL"))
    //          .filter(|path| path.extension().map_or(true, |ext| ext != "wav"))
    //          .sync();

    // while !op.is_done() {
    //     while let Some(entry) = op.read_log() {
    //         println!("{:?} {:?} {}", entry.time, entry.level, entry.message);
    //     }
    //     thread::sleep(Duration::from_millis(100));
    // }
    // return;

    let win = MainWindow::new();
    win.dialog().show_xy(ScreenPosition::Center, ScreenPosition::Center)
                .expect("failed to show the window");
    main_loop();
    return;
}
