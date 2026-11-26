use std::ffi::OsString;
use std::path::PathBuf;

use clap::{ArgGroup, Parser};

/// What the viewer should follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Follow the file named by a state file written by another tool.
    State(PathBuf),
    /// Show a fixed file.
    File(PathBuf),
}

/// Configuration for the TUI viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub source: Source,
    pub on_browse: Option<String>,
}

#[derive(Parser)]
#[command(version, about = "A TUI file viewer")]
#[command(group(
    ArgGroup::new("source")
        .required(true)
        .args(["state", "file"])
))]
struct Args {
    /// State file naming the file to show
    #[arg(long, value_name = "PATH")]
    state: Option<PathBuf>,
    /// Command to run after opening a URL in the browser
    #[arg(long, value_name = "CMD")]
    on_browse: Option<String>,
    /// File to show
    #[arg(value_name = "FILE")]
    file: Option<PathBuf>,
}

pub fn parse_args<I, T>(args: I) -> Result<View, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args = Args::try_parse_from(args)?;
    let absolute = |p: PathBuf| {
        std::path::absolute(&p).map_err(|e| {
            clap::Error::raw(
                clap::error::ErrorKind::Io,
                format!("cannot resolve {}: {e}\n", p.display()),
            )
        })
    };
    let source = match (args.state, args.file) {
        (Some(s), _) => Source::State(absolute(s)?),
        (None, Some(f)) => Source::File(absolute(f)?),
        // The `source` group requires one of them.
        (None, None) => unreachable!("clap requires --state or FILE"),
    };
    Ok(View {
        source,
        on_browse: args.on_browse,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_flag() {
        assert_eq!(
            parse_args(["wtr", "--state", "/tmp/s.json"]).unwrap(),
            View {
                source: Source::State("/tmp/s.json".into()),
                on_browse: None,
            }
        );
    }

    #[test]
    fn positional_file_is_made_absolute() {
        let View {
            source: Source::File(p),
            on_browse: None,
        } = parse_args(["wtr", "a.txt"]).unwrap()
        else {
            panic!()
        };
        assert!(p.is_absolute() && p.ends_with("a.txt"));
    }

    #[test]
    fn on_browse_option() {
        let v = parse_args(["wtr", "a.txt", "--on-browse", "x"]).unwrap();
        assert_eq!(v.on_browse.as_deref(), Some("x"));
    }

    #[test]
    fn neither_is_error() {
        assert!(parse_args(["wtr"]).is_err());
    }

    #[test]
    fn both_is_error() {
        assert!(parse_args(["wtr", "--state", "/s", "a.txt"]).is_err());
    }

    #[test]
    fn removed_history_options_are_errors() {
        assert!(parse_args(["wtr", "--session", "/s.jsonl"]).is_err());
        assert!(parse_args(["wtr", "a", "--icon-width", "2"]).is_err());
    }
}
