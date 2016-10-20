
#![feature(question_mark)]

#[macro_use]
extern crate clear_coat;
extern crate crossbeam;
extern crate itertools;
#[cfg(windows)]
extern crate winapi;
#[cfg(windows)]
extern crate kernel32;

use clear_coat::*;
use clear_coat::common_attrs_cbs::*;

use sync::SyncBuilder;

#[cfg_attr(windows, path = "windows_file_times.rs")]
mod file_times;
mod sync;

fn create_job_page() -> Box<Control> {
    let parallel_jobs_text_box = Text::new();

    let folder_list = List::new();

    let page = vbox!(
        hbox!(&Label::with_title("Parallel Jobs:"), parallel_jobs_text_box),
        &folder_list,
    );

    Box::new(page) as Box<Control>
}

fn create_main_page() -> Box<Control> {
    let job_list = List::new();

    let job_page = create_job_page();

    let sync_button = Button::with_title("Sync");

    let page = vbox!(hbox!(job_list, job_page), hbox!(fill!(), sync_button));

    Box::new(page) as Box<Control>
}

fn main() {
    let dialog = Dialog::new();

    let main_page = create_main_page();

    dialog.append(&*main_page).expect("failed to build the window");
    dialog.set_title("Mirror Sync");

    dialog.show_xy(ScreenPosition::Center, ScreenPosition::Center)
          .expect("failed to show the window");
    main_loop();
}
