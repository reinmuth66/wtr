use std::ffi::OsString;
use std::path::PathBuf;

use clap::{ArgGroup, Parser};

/// What the viewer should follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Follow the file named by a state file written by another tool.
    State(PathBuf),
    /// Show one fixed file.
    File(PathBuf),
}

#[derive(Parser)]
#[command(version, about = "A TUI file viewer")]
#[command(group(ArgGroup::new("source").required(true).args(["state", "file"])))]
struct Args {
    /// State file naming the file to show
    #[arg(long, value_name = "PATH", conflicts_with = "file")]
    state: Option<PathBuf>,
    /// File to show
    #[arg(value_name = "FILE")]
    file: Option<PathBuf>,
}

pub fn parse_args<I, T>(args: I) -> Result<Mode, clap::Error>
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
    match (args.state, args.file) {
        (Some(s), _) => Ok(Mode::State(absolute(s)?)),
        (None, Some(f)) => Ok(Mode::File(absolute(f)?)),
        (None, None) => unreachable!("clap requires one of them"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_flag() {
        assert_eq!(
            parse_args(["wtr", "--state", "/tmp/s.json"]).unwrap(),
            Mode::State("/tmp/s.json".into())
        );
    }

    #[test]
    fn positional_file_is_made_absolute() {
        let Mode::File(p) = parse_args(["wtr", "a.txt"]).unwrap() else {
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
}
