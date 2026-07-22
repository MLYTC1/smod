//! The one place that decides whether ANSI-colored output is enabled.
//!
//! Like the rest of [`crate::ui`], this is presentation-only: business logic
//! never touches it. The binary calls [`init`] exactly once at startup; every
//! other module keeps using [`colored`] as normal and inherits the decision
//! made here (`colored` reads a single process-wide override).
//!
//! Why this exists: on some Windows consoles ANSI escape sequences are printed
//! literally (e.g. `←[1;32m ✓ ←[0m`) unless "virtual terminal processing" is
//! turned on for the console first. Relying on the default detection therefore
//! forced users to pass `--no-color` by hand. [`init`] instead tries to enable
//! that processing and, when it cannot, disables color automatically — while
//! still honoring `--no-color` (and `NO_COLOR`) as an explicit override.

use std::io::IsTerminal;

/// The inputs that determine whether colored output should be enabled.
///
/// Kept as a plain, borrow-free struct so the actual decision
/// ([`should_colorize`]) is a pure function that can be unit-tested without
/// real environment variables or a real terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorEnv {
    /// The `--no-color` flag was passed on the command line.
    pub no_color_flag: bool,
    /// `NO_COLOR` is present in the environment (any value). See
    /// <https://no-color.org>.
    pub no_color_env: bool,
    /// `CLICOLOR_FORCE` is present and set to something other than `"0"`.
    pub clicolor_force: bool,
    /// stdout is connected to an interactive terminal.
    pub stdout_is_terminal: bool,
    /// The terminal is known to interpret ANSI escape sequences.
    pub ansi_supported: bool,
}

/// Decide whether colored output should be enabled, from already-gathered
/// inputs.
///
/// Precedence, highest first:
/// 1. `--no-color` or `NO_COLOR` → always **off** (explicit user override).
/// 2. `CLICOLOR_FORCE` → always **on** (explicit user override).
/// 3. Otherwise **on** only when stdout is a terminal *and* that terminal is
///    known to understand ANSI escape codes.
pub fn should_colorize(env: &ColorEnv) -> bool {
    // Explicit "off" wins over everything, including CLICOLOR_FORCE.
    if env.no_color_flag || env.no_color_env {
        return false;
    }
    if env.clicolor_force {
        return true;
    }
    env.stdout_is_terminal && env.ansi_supported
}

impl ColorEnv {
    /// Gather the real inputs from this process's environment and stdout.
    fn detect(no_color_flag: bool) -> Self {
        let no_color_env = std::env::var_os("NO_COLOR").is_some();
        let clicolor_force = std::env::var_os("CLICOLOR_FORCE")
            .map(|v| v != "0")
            .unwrap_or(false);
        // `TERM=dumb` explicitly declares a terminal with no ANSI support.
        let term_is_dumb = std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false);

        ColorEnv {
            no_color_flag,
            no_color_env,
            clicolor_force,
            stdout_is_terminal: std::io::stdout().is_terminal(),
            ansi_supported: !term_is_dumb && detect_ansi_support(),
        }
    }
}

/// Configure process-wide colored output based on the `--no-color` flag and the
/// current environment. Call once, at startup, before any colored output.
pub fn init(no_color_flag: bool) {
    let env = ColorEnv::detect(no_color_flag);
    // Set the override in both directions rather than only for the "off" case,
    // so our decision — not `colored`'s built-in guess — is authoritative.
    colored::control::set_override(should_colorize(&env));
}

/// Detect whether the current terminal understands ANSI escape sequences.
///
/// On Unix a real terminal effectively always does. On Windows, modern
/// consoles support ANSI once "virtual terminal processing" is enabled, so this
/// tries to enable it and reports whether that succeeded.
fn detect_ansi_support() -> bool {
    #[cfg(windows)]
    {
        windows::enable_ansi_support()
    }
    #[cfg(not(windows))]
    {
        true
    }
}

#[cfg(windows)]
mod windows {
    //! Minimal, dependency-free FFI to turn on ANSI escape-sequence processing
    //! for the current stdout console (`ENABLE_VIRTUAL_TERMINAL_PROCESSING`).

    use core::ffi::c_void;

    type Handle = *mut c_void;
    type Dword = u32;
    type Bool = i32;

    // `(DWORD)-11`, expressed directly to avoid a signed-to-unsigned cast.
    const STD_OUTPUT_HANDLE: Dword = 0xFFFF_FFF5;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: Dword = 0x0004;

    extern "system" {
        fn GetStdHandle(nStdHandle: Dword) -> Handle;
        fn GetConsoleMode(hConsoleHandle: Handle, lpMode: *mut Dword) -> Bool;
        fn SetConsoleMode(hConsoleHandle: Handle, dwMode: Dword) -> Bool;
    }

    /// Try to enable ANSI processing on stdout. Returns `true` if the console
    /// now understands ANSI codes (either because we enabled it or it was
    /// already on), and `false` if stdout is not a console we can configure.
    pub fn enable_ansi_support() -> bool {
        // SAFETY: these are documented Win32 console APIs with matching
        // signatures. We only read/write one local `mode` value and pass a
        // handle obtained from `GetStdHandle`; no memory is aliased or freed.
        unsafe {
            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            if handle.is_null() || handle == (-1isize as Handle) {
                return false;
            }
            let mut mode: Dword = 0;
            if GetConsoleMode(handle, &mut mode) == 0 {
                // Not a real console (e.g. redirected to a file or pipe).
                return false;
            }
            if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0 {
                return true; // Already enabled.
            }
            SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A terminal that fully supports ANSI, as the baseline to tweak per test.
    fn ansi_terminal() -> ColorEnv {
        ColorEnv {
            no_color_flag: false,
            no_color_env: false,
            clicolor_force: false,
            stdout_is_terminal: true,
            ansi_supported: true,
        }
    }

    #[test]
    fn colors_on_for_ansi_capable_terminal() {
        assert!(should_colorize(&ansi_terminal()));
    }

    #[test]
    fn no_color_flag_disables_colors() {
        let env = ColorEnv {
            no_color_flag: true,
            ..ansi_terminal()
        };
        assert!(!should_colorize(&env));
    }

    #[test]
    fn no_color_env_disables_colors() {
        let env = ColorEnv {
            no_color_env: true,
            ..ansi_terminal()
        };
        assert!(!should_colorize(&env));
    }

    #[test]
    fn unsupported_terminal_disables_colors_automatically() {
        // The core bug: a terminal that does not process ANSI must not get
        // colored output, without the user passing --no-color.
        let env = ColorEnv {
            ansi_supported: false,
            ..ansi_terminal()
        };
        assert!(!should_colorize(&env));
    }

    #[test]
    fn non_terminal_stdout_disables_colors() {
        // Piped/redirected output should not carry ANSI codes.
        let env = ColorEnv {
            stdout_is_terminal: false,
            ..ansi_terminal()
        };
        assert!(!should_colorize(&env));
    }

    #[test]
    fn clicolor_force_enables_colors_even_when_piped() {
        let env = ColorEnv {
            clicolor_force: true,
            stdout_is_terminal: false,
            ansi_supported: false,
            ..ansi_terminal()
        };
        assert!(should_colorize(&env));
    }

    #[test]
    fn explicit_no_color_beats_clicolor_force() {
        // An explicit "off" must win over a force-on request.
        let env = ColorEnv {
            no_color_flag: true,
            clicolor_force: true,
            ..ansi_terminal()
        };
        assert!(!should_colorize(&env));

        let env = ColorEnv {
            no_color_env: true,
            clicolor_force: true,
            ..ansi_terminal()
        };
        assert!(!should_colorize(&env));
    }
}
