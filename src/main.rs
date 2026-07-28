mod app;
mod data;
mod monitor;

use app::RgmApp;
use eframe::egui::ViewportBuilder;

/// What the command line asked for.
enum Startup {
    /// Open the window.
    Run,
    /// Write this to stdout and exit successfully.
    Print(String),
    /// Write this to stderr and exit with a usage error.
    Reject(String),
}

fn version() -> String {
    format!("rgm {}", env!("CARGO_PKG_VERSION"))
}

fn usage() -> String {
    format!(
        "{}\n\
         {}\n\
         \n\
         Usage: rgm [OPTIONS]\n\
         \n\
         Options:\n    \
             -h, --help     Print this help and exit\n    \
             -V, --version  Print the version and exit\n\
         \n\
         With no options rgm opens a window showing GPU utilization, memory,\n\
         temperature, power, clock-throttle state and the processes holding\n\
         video memory. It takes no other arguments.",
        version(),
        env!("CARGO_PKG_DESCRIPTION"),
    )
}

/// `args` is everything after argv[0].
///
/// Anything unrecognised is rejected rather than ignored: silently opening a
/// window in response to `rgm --verison` leaves the user with no idea the flag
/// was wrong, and this binary previously did exactly that for every argument.
fn parse_args<I, S>(args: I) -> Startup
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let reject = |arg: &str| {
        Startup::Reject(format!(
            "rgm: unexpected argument '{arg}'\nTry 'rgm --help' for usage."
        ))
    };

    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Startup::Run;
    };
    let action = match first.as_ref() {
        "-V" | "--version" => Startup::Print(version()),
        "-h" | "--help" => Startup::Print(usage()),
        other => return reject(other),
    };

    // Every accepted flag is terminal, so a second argument was never going to
    // do anything. Say so rather than dropping it.
    match args.next() {
        Some(extra) => reject(extra.as_ref()),
        None => action,
    }
}

fn main() {
    match parse_args(std::env::args().skip(1)) {
        Startup::Run => {}
        Startup::Print(text) => {
            println!("{text}");
            return;
        }
        Startup::Reject(text) => {
            eprintln!("{text}");
            std::process::exit(2);
        }
    }

    let native_options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size([1000.0, 700.0])
            // Without this, egui-winit never calls set_app_id on Wayland, so
            // the window has no app_id for compositors to match against
            // rgm.desktop or a tiling rule. Must equal the desktop file's
            // basename.
            //
            // It is not free on X11: egui-winit passes the id through as
            // winit's shared `name` field with an empty instance, so WM_CLASS
            // goes from ("rgm", "rgm") to ("", "rgm"). Rules matching the
            // class still work, ones matching the instance do not, and eframe
            // exposes no way to set them separately.
            .with_app_id("rgm"),
        ..Default::default()
    };

    eframe::run_native(
        "RGM",
        native_options,
        Box::new(|cc| Ok(Box::new(RgmApp::new(cc)))),
    )
    .expect("Failed to start application");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn printed(args: &[&str]) -> String {
        match parse_args(args.iter().copied()) {
            Startup::Print(text) => text,
            Startup::Run => panic!("{args:?} opened the window"),
            Startup::Reject(text) => panic!("{args:?} was rejected: {text}"),
        }
    }

    #[test]
    fn no_arguments_opens_the_window() {
        assert!(matches!(
            parse_args(std::iter::empty::<&str>()),
            Startup::Run
        ));
    }

    #[test]
    fn version_is_reported_without_opening_a_window() {
        let expected = format!("rgm {}", env!("CARGO_PKG_VERSION"));
        assert_eq!(printed(&["--version"]), expected);
        assert_eq!(printed(&["-V"]), expected);
    }

    #[test]
    fn help_describes_both_flags() {
        for text in [printed(&["--help"]), printed(&["-h"])] {
            assert!(text.contains("Usage: rgm"), "got {text:?}");
            assert!(text.contains("--version"), "got {text:?}");
            assert!(text.contains("--help"), "got {text:?}");
        }
    }

    #[test]
    fn an_unrecognised_argument_is_not_silently_ignored() {
        // The defect this replaces: every argument, including a typo, used to
        // fall through and open the GUI.
        for arg in ["--verison", "-x", "--", "status", "-"] {
            match parse_args([arg]) {
                Startup::Reject(text) => {
                    assert!(text.contains(arg), "got {text:?}");
                    assert!(text.contains("--help"), "got {text:?}");
                }
                _ => panic!("{arg:?} should not have been accepted"),
            }
        }
    }

    #[test]
    fn a_trailing_argument_is_reported_rather_than_dropped() {
        // Each accepted flag is terminal, so a second one would have been
        // silently discarded.
        match parse_args(["--version", "--help"]) {
            Startup::Reject(text) => assert!(text.contains("--help"), "got {text:?}"),
            _ => panic!("the trailing argument should not have been accepted"),
        }
        match parse_args(["--help", "extra"]) {
            Startup::Reject(text) => assert!(text.contains("extra"), "got {text:?}"),
            _ => panic!("the trailing argument should not have been accepted"),
        }
    }
}
