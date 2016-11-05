
#![feature(question_mark)]

#[macro_use]
extern crate clear_coat;
extern crate crossbeam;
extern crate itertools;
#[cfg(windows)]
extern crate winapi;
#[cfg(windows)]
extern crate kernel32;

use std::cell::RefCell;
use std::rc::Rc;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use clear_coat::*;
use clear_coat::common_attrs_cbs::*;

use sync::SyncBuilder;

#[cfg_attr(windows, path = "windows_file_times.rs")]
mod file_times;
mod sync;

struct Job {
    name: String,
    parallel_copies: u8,
}

struct JobPageData {
    control: Vbox,
    name_text_box: Text,
    parallel_copies_text_box: Text,
}

struct MainWindowData {
    jobs: RefCell<Vec<Job>>,

    dialog: Dialog,
    job_list: List,
    job_page: JobPageData,
}

const NAME_VISIBLE_COLUMNS: u32 = 15;

#[derive(Clone)]
struct MainWindow(Rc<MainWindowData>);

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

        let main_window_zyg = MainWindow(Rc::new(MainWindowData {
            jobs: RefCell::new(vec![]),
            dialog: dialog,
            job_list: job_list,
            job_page: job_page,
        }));

        let main_window = main_window_zyg.clone();
        main_window_zyg.0.job_list.action_event().add(move |_: &ListActionArgs|
            main_window.update_job_page()
        );
        let main_window = main_window_zyg.clone();
        add_job_button.action_event().add(move || main_window.add_new_job());

        let main_window = main_window_zyg.clone();
        main_window_zyg.0.job_page.name_text_box.value_changed_event().add(move ||
            if let Some(sel_index) = main_window.0.job_list.value_single() {
                {
                    let mut jobs = main_window.0.jobs.borrow_mut();
                    jobs[sel_index].name = main_window.0.job_page.name_text_box.value();
                }
                main_window.update_job_list();
            }
        );

        let main_window = main_window_zyg.clone();
        main_window_zyg.0.job_page.parallel_copies_text_box.value_changed_event().add(move ||
            if let Some(sel_index) = main_window.0.job_list.value_single() {
                let mut jobs = main_window.0.jobs.borrow_mut();
                let parallel_str = main_window.0.job_page.parallel_copies_text_box.value();
                if let Ok(parallel_copies) = parallel_str.parse::<u8>() {
                    jobs[sel_index].parallel_copies = parallel_copies;
                }
            }
        );

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
                vbox!(&Label::with_title("Blacklist"), &blacklist),
            ),
        );

        JobPageData {
            name_text_box: name_text_box,
            parallel_copies_text_box: parallel_copies_text_box,
            control: page,
        }
    }

    pub fn dialog(&self) -> &Dialog {
        &self.0.dialog
    }

    fn update_job_page(&self) {
        let sel_index = self.0.job_list.value_single();
        if let Some(sel_index) = sel_index {
            let jobs = self.0.jobs.borrow();
            self.0.job_page.name_text_box.set_value(&jobs[sel_index].name);
            self.0.job_page.parallel_copies_text_box.set_value(&jobs[sel_index].parallel_copies.to_string());
        }
    }

    fn update_job_list(&self) {
        let sel_index = self.0.job_list.value_single();
        let jobs = self.0.jobs.borrow();
        self.0.job_list.set_items(jobs.iter().map(|job| &job.name));
        self.0.job_list.set_value_single(sel_index);
    }

    fn add_new_job(&self) {
        {
            let mut jobs = self.0.jobs.borrow_mut();
            jobs.push(Job {
                name: "Unnamed".into(),
                parallel_copies: 2,
            });
        }
        self.update_job_list();
        // TODO: I hate all this RefCell borrowing. I need to figure out a pattern to reduce it.
        self.0.job_list.set_value_single(Some(self.0.jobs.borrow().len() - 1));
        self.update_job_page();
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
