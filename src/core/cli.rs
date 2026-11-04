//! The command line: which argument is the file or folder to open.
//!
//! Kept in the library rather than in the binary because this rule has already
//! been wrong once and the cost was a wrong measurement, not a crash. See
//! [`path_arg`].

/// The file or folder to open, taken from the command line: the first argument
/// that is not a flag.
///
/// **Not `args().nth(1)`.** That is wrong the moment a flag can come first, and
/// it fails *silently and expensively*: `--perf` is not a path, so the app fell
/// through to the persisted folder and quietly measured or displayed the wrong
/// thing. `--perf <folder>` is the documented invocation, so a flag's own
/// argument has to survive this filter - and `--perf` takes no value, so its
/// folder is simply the first non-flag argument.
///
/// An argument is treated as a flag when it starts with `-`. On Windows that is
/// safe for real paths; if it ever is not, `.\-name.jpg` still works.
///
/// **A flag that consumes a value which is not itself a path** (a backend name,
/// say) would be handed back as the folder here, because only `-` marks a flag.
/// Handle those before this is called: `--register`, `--unregister` and
/// `--icc-scan <folder>` all return before the window opens.
pub fn path_arg<I, S>(args: I) -> Option<std::path::PathBuf>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .map(|s| s.as_ref().to_string())
        .find(|s| !s.starts_with('-') && !s.is_empty())
        .map(std::path::PathBuf::from)
}
