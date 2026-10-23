use std::ffi::OsString;
use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use parch_core::threads;
use parch_core::util::parse_size;

/// Extensions stored without compression by default (already compressed formats).
pub const DEFAULT_STORE_EXT: &str = "7z,aac,apk,avi,br,bz2,cab,docx,epub,flac,gif,gz,heic,heif,jar,jpeg,jpg,lz,lz4,lzma,\
m4a,m4v,mkv,mov,mp3,mp4,odp,ods,odt,ogg,opus,png,pptx,rar,tbz,tgz,txz,webm,webp,whl,woff,woff2,xlsx,xz,zip,zst";

#[derive(Parser)]
#[command(name = "parch", version, about = "Parallel ZIP archiver", propagate_version = true)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Args, Clone)]
pub struct Global {
    /// Worker threads: N or P% of available cores [default: 75%]
    #[arg(short = 'j', long, global = true, help_heading = "Global options", env = "PARCH_THREADS", value_parser = threads::parse_threads, value_name = "N|P%")]
    pub threads: Option<usize>,
    /// Print only errors
    #[arg(short, long, global = true, help_heading = "Global options")]
    pub quiet: bool,
    /// Print every entry (repeatable)
    #[arg(short, long, global = true, help_heading = "Global options", action = ArgAction::Count)]
    pub verbose: u8,
    /// Force the progress bar on stderr
    #[arg(long, global = true, help_heading = "Global options", overrides_with = "no_progress")]
    pub progress: bool,
    /// Disable the progress bar
    #[arg(long, global = true, help_heading = "Global options")]
    pub no_progress: bool,
    /// Memory budget for data in flight
    #[arg(long, global = true, help_heading = "Global options", value_parser = parse_size, default_value = "256M", value_name = "SIZE")]
    pub memory: u64,
    /// Archive format [default: by extension or signature]
    #[arg(long, global = true, help_heading = "Global options", value_name = "FMT")]
    pub format: Option<String>,
}

impl Global {
    pub fn threads(&self) -> usize {
        self.threads.unwrap_or_else(threads::default_threads)
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// Create an archive
    #[command(visible_alias = "c")]
    Create(CreateArgs),
    /// Extract entries
    #[command(visible_alias = "x")]
    Extract(ExtractArgs),
    /// List entries
    #[command(visible_alias = "l")]
    List(ListArgs),
    /// Verify entries
    #[command(visible_alias = "t")]
    Test(TestArgs),
    /// Add or replace entries in an archive
    #[command(visible_alias = "a")]
    Add(AddArgs),
    /// Delete entries from an archive
    #[command(visible_alias = "d")]
    Delete(DeleteArgs),
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Method {
    Store,
    Deflate,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Zip64 {
    Auto,
    Always,
}

#[derive(Args, Clone)]
pub struct CompressArgs {
    /// Compression level 0-12 (0 = store, 10-12 = slower and denser); -0 ... -9 also work
    #[arg(short = 'l', long, value_parser = clap::value_parser!(u32).range(0..=12), value_name = "N")]
    pub level: Option<u32>,
    /// Compression method
    #[arg(short = 'm', long, value_enum)]
    pub method: Option<Method>,
    /// Change to DIR before adding paths
    #[arg(short = 'C', long = "directory", value_name = "DIR")]
    pub directory: Option<PathBuf>,
    /// Exclude names matching GLOB (repeatable)
    #[arg(short = 'x', long, value_name = "GLOB")]
    pub exclude: Vec<String>,
    /// Include only names matching GLOB (repeatable)
    #[arg(short = 'i', long, value_name = "GLOB")]
    pub include: Vec<String>,
    /// Read exclude patterns from FILE
    #[arg(long, value_name = "FILE")]
    pub exclude_from: Vec<PathBuf>,
    /// Read paths to add from FILE ('-' for stdin)
    #[arg(short = 'T', long, value_name = "FILE")]
    pub files_from: Option<PathBuf>,
    /// Paths in --files-from are NUL-separated
    #[arg(long)]
    pub null: bool,
    /// Follow symbolic links instead of storing them
    #[arg(short = 'L', long)]
    pub follow_symlinks: bool,
    /// Do not add directory entries
    #[arg(short = 'D', long)]
    pub no_dir_entries: bool,
    /// Do not store extra attributes (precise times, uid/gid)
    #[arg(short = 'X', long)]
    pub no_extra: bool,
    /// Extensions stored without compression (comma-separated; empty to disable)
    #[arg(long, value_name = "EXT,...")]
    pub store_ext: Option<String>,
    /// Write entries in sorted order (output independent of thread count)
    #[arg(long)]
    pub sort: bool,
    /// Reproducible archive: --sort, -X, normalized permissions, UTC times, SOURCE_DATE_EPOCH
    #[arg(long)]
    pub reproducible: bool,
    /// When to use ZIP64 structures
    #[arg(long, value_enum, default_value = "auto")]
    pub zip64: Zip64,
    /// Chunk size for parallel compression of big files
    #[arg(long, value_parser = parse_size, default_value = "1M", value_name = "SIZE")]
    pub chunk_size: u64,
    /// Files larger than this are compressed in parallel chunks
    #[arg(long, value_parser = parse_size, default_value = "8M", value_name = "SIZE")]
    pub big_file: u64,
    /// Archive comment
    #[arg(long, value_name = "TEXT")]
    pub comment: Option<String>,
    /// Store non-ASCII names in a legacy charset (e.g. cp866) with a Unicode Path field
    #[arg(long, value_name = "CP")]
    pub charset: Option<String>,
}

#[derive(Args, Clone)]
pub struct CreateArgs {
    /// Archive to create ('-' for stdout)
    pub archive: PathBuf,
    /// Files and directories to add
    pub paths: Vec<PathBuf>,
    #[command(flatten)]
    pub opts: CompressArgs,
}

#[derive(Args, Clone)]
pub struct AddArgs {
    /// Archive to update (created if missing)
    pub archive: PathBuf,
    /// Files and directories to add
    pub paths: Vec<PathBuf>,
    #[command(flatten)]
    pub opts: CompressArgs,
    /// Add new files and replace entries only when the file is newer
    #[arg(short = 'u', long, conflicts_with = "freshen")]
    pub update: bool,
    /// Replace existing entries when the file is newer; add nothing new
    #[arg(short = 'f', long)]
    pub freshen: bool,
}

#[derive(Args, Clone)]
pub struct ExtractArgs {
    /// Archive to extract ('-' for stdin)
    pub archive: PathBuf,
    /// Extract only entries matching these globs
    pub patterns: Vec<String>,
    /// Destination directory
    #[arg(short = 'C', long = "directory", default_value = ".", value_name = "DIR")]
    pub directory: PathBuf,
    /// Skip entries matching GLOB (repeatable)
    #[arg(short = 'x', long, value_name = "GLOB")]
    pub exclude: Vec<String>,
    /// Overwrite existing files
    #[arg(short = 'o', long, conflicts_with = "update")]
    pub overwrite: bool,
    /// Overwrite only files older than the entry
    #[arg(short = 'u', long)]
    pub update: bool,
    /// Drop N leading path components
    #[arg(long, default_value_t = 0, value_name = "N")]
    pub strip_components: usize,
    /// Extract all files into the destination without their directories
    #[arg(long)]
    pub flat: bool,
    /// Write file contents to stdout
    #[arg(short = 'p', long)]
    pub to_stdout: bool,
    /// Restore owner and group (needs privileges)
    #[arg(long)]
    pub same_owner: bool,
    /// Do not restore permissions
    #[arg(long)]
    pub no_perms: bool,
    /// Do not restore modification times
    #[arg(long)]
    pub no_times: bool,
    /// Allow symlinks pointing outside the destination and writing through symlinked directories
    #[arg(long)]
    pub allow_unsafe_symlinks: bool,
    /// Charset of names not marked as UTF-8 (e.g. cp866)
    #[arg(short = 'O', long, value_name = "CP")]
    pub charset: Option<String>,
}

#[derive(Args, Clone)]
pub struct ListArgs {
    /// Archive to list ('-' for stdin)
    pub archive: PathBuf,
    /// List only entries matching these globs
    pub patterns: Vec<String>,
    /// Charset of names not marked as UTF-8 (e.g. cp866)
    #[arg(short = 'O', long, value_name = "CP")]
    pub charset: Option<String>,
}

#[derive(Args, Clone)]
pub struct TestArgs {
    /// Archive to verify ('-' for stdin)
    pub archive: PathBuf,
    /// Verify only entries matching these globs
    pub patterns: Vec<String>,
    /// Skip entries matching GLOB (repeatable)
    #[arg(short = 'x', long, value_name = "GLOB")]
    pub exclude: Vec<String>,
    /// Charset of names not marked as UTF-8 (e.g. cp866)
    #[arg(short = 'O', long, value_name = "CP")]
    pub charset: Option<String>,
}

#[derive(Args, Clone)]
pub struct DeleteArgs {
    /// Archive to modify
    pub archive: PathBuf,
    /// Globs of entries to delete
    #[arg(required = true)]
    pub patterns: Vec<String>,
    /// Charset of names not marked as UTF-8 (e.g. cp866)
    #[arg(short = 'O', long, value_name = "CP")]
    pub charset: Option<String>,
}

/// Rewrites `-0` ... `-9` into `--level=N` (before a `--` separator).
pub fn preprocess(args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut out = Vec::new();
    let mut literal = false;
    for a in args {
        if !literal {
            if a == "--" {
                literal = true;
            } else if let Some(s) = a.to_str() {
                let b = s.as_bytes();
                if b.len() == 2 && b[0] == b'-' && b[1].is_ascii_digit() {
                    out.push(OsString::from(format!("--level={}", b[1] - b'0')));
                    continue;
                }
            }
        }
        out.push(a);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn digit_flags_become_levels() {
        let v = preprocess(["parch", "c", "-9", "a.zip", "--", "-1"].map(OsString::from));
        assert_eq!(v, ["parch", "c", "--level=9", "a.zip", "--", "-1"].map(OsString::from));
    }
}
