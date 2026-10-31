//! `herdr notes hook` (fork): Claude's `SessionStart` notes recall hook
//! (`crate::notes::recall`). The other `herdr notes …` verbs and
//! `herdr checkpoint …` were removed: the notes page is the user's, edited
//! in the info pane, and agents use the notes tools.

const NOTES_USAGE: &str = "usage: herdr notes hook   (Claude's SessionStart recall hook: reads stdin, always exits 0)\nThe notes page is edited in the info pane; agents use agents_notes_read / agents_notes_write.";

pub(super) fn run_notes_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        // Claude's SessionStart hook (the wrap's settings file): never parses
        // its arguments and always exits 0, so it cannot block a session.
        Some("hook") => {
            crate::notes::recall::run_hook();
            Ok(0)
        }
        Some("help" | "--help" | "-h") => {
            println!("{NOTES_USAGE}");
            Ok(0)
        }
        _ => {
            eprintln!("{NOTES_USAGE}");
            Ok(2)
        }
    }
}
