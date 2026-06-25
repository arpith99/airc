use chrono::Local;
use colored::Colorize;
use std::io::{Write, stdout};

pub(crate) fn print_timestamp() {
    let local = Local::now();
    print!(
        "{}",
        format!("{}", local.format("[%H:%M:%S] ")).bright_black()
    );
}

pub(crate) fn print_sent_line(line: &str) {
    print_timestamp();
    print!("{}", format!("{}: ", "TX").red());
    print_line(line, false);
}

pub(crate) fn print_received_line(line: &str) {
    print_timestamp();
    print!("{}", format!("{}: ", "RX").green());
    print_line(line, false);
}

pub(crate) fn print_line(line: &str, ts_flag: bool) {
    let colon_index = line.find(" :").unwrap_or(0);
    let prefix = &line[..colon_index];
    let message = &line[colon_index..];
    if ts_flag {
        print_timestamp();
    }
    print!("{}", prefix.yellow());
    print!("{}", message.white());
    let _ = stdout().flush(); // Ignore flush errors (e.g., terminal closed)
}
