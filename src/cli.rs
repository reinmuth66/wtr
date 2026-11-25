use std::ffi::OsString;
use std::path::PathBuf;

use clap::{ArgGroup, Parser};

/// What the viewer should follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Follow the file and session named by a state file written by another tool.
    State(PathBuf),
    /// Show fixed file and/or session paths.
    Fixed {
        file: Option<PathBuf>,
        session: Option<PathBuf>,
    },
}

/// Configuration for the TUI viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub source: Source,
    pub on_browse: Option<String>,
    pub icon_width: u16,
}

/// Top-level CLI command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    View(View),
    ScanRecords(Vec<PathBuf>),
}

#[derive(Parser)]
#[command(version, about = "A TUI file viewer")]
#[command(group(
    ArgGroup::new("source")
        .required(true)
        .multiple(true)
        .args(["state", "file", "session"])
))]
struct Args {
    /// State file naming the file and session to show
    #[arg(long, value_name = "PATH", conflicts_with_all = ["file", "session"])]
    state: Option<PathBuf>,
    /// Session JSONL file to show
    #[arg(long, value_name = "PATH")]
    session: Option<PathBuf>,
    /// Command to run after opening a URL in the browser
    #[arg(long, value_name = "CMD")]
    on_browse: Option<String>,
    /// Display width of Nerd Font icons (1 or 2)
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..=2))]
    icon_width: u16,
    /// File to show
    #[arg(value_name = "FILE")]
    file: Option<PathBuf>,
}

pub fn parse_args<I, T>(args: I) -> Result<Command, clap::Error>
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
    let source = match (args.state, args.file, args.session) {
        (Some(s), _, _) => Source::State(absolute(s)?),
        (None, f, s) => Source::Fixed {
            file: f.map(absolute).transpose()?,
            session: s.map(absolute).transpose()?,
        },
    };
    Ok(Command::View(View {
        source,
        on_browse: args.on_browse,
        icon_width: args.icon_width,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_flag() {
        assert_eq!(
            parse_args(["wtr", "--state", "/tmp/s.json"]).unwrap(),
            Command::View(View {
                source: Source::State("/tmp/s.json".into()),
                on_browse: None,
                icon_width: 1,
            })
        );
    }

    #[test]
    fn positional_file_is_made_absolute() {
        let Command::View(View {
            source:
                Source::Fixed {
                    file: Some(p),
                    session: None,
                },
            on_browse: None,
            icon_width: 1,
        }) = parse_args(["wtr", "a.txt"]).unwrap()
        else {
            panic!()
        };
        assert!(p.is_absolute() && p.ends_with("a.txt"));
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
    fn session_flag_and_options() {
        let Command::View(v) = parse_args([
            "wtr",
            "--session",
            "s.jsonl",
            "--icon-width",
            "2",
            "--on-browse",
            "x",
        ])
        .unwrap() else {
            panic!()
        };
        let Source::Fixed {
            file: None,
            session: Some(s),
        } = v.source
        else {
            panic!()
        };
        assert!(s.is_absolute() && s.ends_with("s.jsonl"));
        assert_eq!(v.on_browse.as_deref(), Some("x"));
        assert_eq!(v.icon_width, 2);
    }

    #[test]
    fn file_and_session_together() {
        let Command::View(View {
            source:
                Source::Fixed {
                    file: Some(f),
                    session: Some(s),
                },
            ..
        }) = parse_args(["wtr", "a.txt", "--session", "s.jsonl"]).unwrap()
        else {
            panic!()
        };
        assert!(f.is_absolute() && f.ends_with("a.txt"));
        assert!(s.is_absolute() && s.ends_with("s.jsonl"));
    }

    #[test]
    fn state_conflicts_with_session() {
        assert!(parse_args(["wtr", "--state", "/s", "--session", "/x"]).is_err());
    }

    #[test]
    fn icon_width_must_be_1_or_2() {
        assert!(parse_args(["wtr", "a", "--icon-width", "3"]).is_err());
        assert!(parse_args(["wtr", "a", "--icon-width", "0"]).is_err());
    }
}
