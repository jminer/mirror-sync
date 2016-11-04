
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
use std::path::PathBuf;
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
    parallel_jobs_text_box: Text,
}

struct MainWindowData {
    jobs: RefCell<Vec<Job>>,

    dialog: Dialog,
    job_list: List,
    job_page: JobPageData,
}

#[derive(Clone)]
struct MainWindow(Rc<MainWindowData>);

impl MainWindow {
    pub fn new() -> Self {
        let dialog = Dialog::new();

        let job_list_label = Label::with_title("Jobs");
        let job_list = List::new();
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
        dialog.append(&main_page).expect("failed to build the window");
        dialog.set_title("Mirror Sync");

        let main_window = MainWindow(Rc::new(MainWindowData {
            jobs: RefCell::new(vec![]),
            dialog: dialog,
            job_list: job_list,
            job_page: job_page,
        }));

        let main_window_capt = main_window.clone();
        main_window.0.job_list.action_event().add(move |_: &ListActionArgs| main_window_capt.update_job_page());
        let main_window_capt = main_window.clone();
        add_job_button.action_event().add(move || main_window_capt.add_new_job());

        main_window
    }

    fn create_job_page() -> JobPageData {
        let parallel_jobs_text_box = Text::new();

        let folder_list = List::new();

        let blacklist = List::new();

        let page = vbox!(
            hbox!(&Label::with_title("Parallel Jobs:"), &parallel_jobs_text_box),
            hbox!(
                vbox!(&folder_list),
                vbox!(&blacklist),
            ),
        );

        JobPageData {
            parallel_jobs_text_box: parallel_jobs_text_box,
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
            self.0.job_page.parallel_jobs_text_box.set_value(&jobs[sel_index].parallel_copies.to_string());
        }
    }

    fn add_new_job(&self) {
        let mut jobs = self.0.jobs.borrow_mut();
        jobs.push(Job {
            name: "Unnamed".into(),
            parallel_copies: 2,
        });
        self.0.job_list.set_items(jobs.iter().map(|job| &job.name));
        self.0.job_list.set_value_single(Some(jobs.len() - 1));
    }
}

fn main() {
    let win = MainWindow::new();
    win.dialog().show_xy(ScreenPosition::Center, ScreenPosition::Center)
                .expect("failed to show the window");
    main_loop();
    return;
}
