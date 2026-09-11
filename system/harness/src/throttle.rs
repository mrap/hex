//! Self-throttle module for long-running background maintenance commands.
//!
//! Lowers the *process'* OS scheduling priority to BACKGROUND so the whole
//! process (every thread + IO) is deprioritized at once. Pairs with the
//! OBS-019 cap on ORT op-threads.
//!
//! See me/decisions/consolidate-cpu-throttle-2026-06-04.md.

/// Pure predicate: do we throttle (lower priority) given the --max flag?
pub fn should_throttle(max: bool) -> bool {
    !max
}

/// Lower the current process to background scheduling priority.
///
/// macOS:   PRIO_DARWIN_PROCESS (=4) + PRIO_DARWIN_BG (=0x1000)
/// linux:   setpriority(PRIO_PROCESS, 0, 10)  (nice 10)
/// other:   no-op, returns Ok(())
pub fn lower_to_background() -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        // PRIO_DARWIN_PROCESS = 4, PRIO_DARWIN_BG = 0x1000.
        // setpriority returns 0 on success, -1 on error.
        // Prefer Darwin background QoS (throttles CPU *and* IO). If that's
        // unavailable, fall back to a plain nice bump (CPU only) before giving
        // up — strictly better than running at normal priority.
        let bg = unsafe { libc::setpriority(4, 0, 0x1000) };
        if bg == 0 {
            return Ok(());
        }
        let nice = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 10) };
        if nice == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(target_os = "linux")]
    {
        let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 10) };
        if rc == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Ok(())
    }
}

/// Apply throttling based on the --max flag. Prints loudly (Rule P2/S6).
/// `task` labels the message so each background command (consolidate, memory
/// index, …) reports its own throttle state.
pub fn apply(task: &str, max: bool) {
    if should_throttle(max) {
        match lower_to_background() {
            Ok(()) => {
                println!(
                    "{task}: running at throttled (background) priority — pass --max to run at full speed"
                );
            }
            Err(e) => {
                eprintln!(
                    "{task}: WARN could not lower priority: {e} — continuing at normal priority"
                );
            }
        }
    } else {
        println!("{task}: running at MAX priority (--max)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_child;

    fn observed_priority_state() -> std::io::Result<String> {
        #[cfg(target_os = "macos")]
        unsafe {
            *libc::__error() = 0;
            let background = libc::getpriority(4, 0);
            if *libc::__error() != 0 {
                return Err(std::io::Error::last_os_error());
            }
            *libc::__error() = 0;
            let nice = libc::getpriority(libc::PRIO_PROCESS, 0);
            if *libc::__error() != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(format!(
                "darwin-priority-selector4={background}; nice={nice}"
            ))
        }
        #[cfg(target_os = "linux")]
        unsafe {
            *libc::__errno_location() = 0;
            let nice = libc::getpriority(libc::PRIO_PROCESS, 0);
            if *libc::__errno_location() != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(format!("nice={nice}"))
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        Ok("unsupported-platform".to_string())
    }

    fn run_priority_in_exact_child(name: &str) -> bool {
        if test_child::in_child(name) {
            test_child::stage("throttle:child-entry").expect("test-child stage must flush");
            return false;
        }
        let before = observed_priority_state().expect("parent priority query before child");
        let home = tempfile::tempdir().expect("private priority HOME");
        let output = test_child::run_exact(name, home.path(), home.path(), test_child::Fault::None)
            .unwrap_or_else(|error| panic!("private priority test {name} failed: {error}"));
        let stdout = String::from_utf8_lossy(&output.stdout.tail);
        assert!(
            stdout.contains("THROTTLE_CHILD_BEFORE:"),
            "child must retain its pre-state: {stdout}"
        );
        assert!(
            stdout.contains("THROTTLE_CHILD_BRANCH:ok")
                || stdout.contains("THROTTLE_CHILD_BRANCH:permission-denied"),
            "child must report the actual syscall branch: {stdout}"
        );
        assert!(
            stdout.contains("THROTTLE_CHILD_STATE:"),
            "child must retain its post-state: {stdout}"
        );
        test_child::require_one_pass(name, output).unwrap_or_else(|error| panic!("{error}"));
        let after = observed_priority_state().expect("parent priority query after child");
        assert_eq!(
            before, after,
            "the exact child must not change parent priority state; before={before}; after={after}"
        );
        println!("HEX_PRIORITY_PARENT_STATE test={name} before={before} after={after}");
        true
    }

    #[test]
    fn should_throttle_max_true_is_false() {
        assert!(!should_throttle(true));
    }

    #[test]
    fn should_throttle_max_false_is_true() {
        assert!(should_throttle(false));
    }

    #[test]
    fn lower_to_background_is_ok_or_permission_denied() {
        const NAME: &str = "throttle::tests::lower_to_background_is_ok_or_permission_denied";
        if run_priority_in_exact_child(NAME) {
            return;
        }
        test_child::stage("throttle:before-syscall").expect("test-child stage must flush");
        println!(
            "THROTTLE_CHILD_BEFORE:{}",
            observed_priority_state().expect("child priority query before syscall")
        );
        // In a normal process (launchd job, plain shell) lowering priority is
        // always permitted unprivileged and returns Ok. In a *restricted*
        // context — a sandboxed CI runner or the agent's sandboxed shell —
        // setpriority can be blocked outright and returns EPERM. Both are
        // acceptable: `apply()` degrades gracefully on EPERM. The contract is
        // only that this never panics and never returns some *other* error.
        match lower_to_background() {
            Ok(()) => println!("THROTTLE_CHILD_BRANCH:ok"),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                println!("THROTTLE_CHILD_BRANCH:permission-denied");
            }
            Err(e) => panic!("unexpected error kind from lower_to_background: {e:?}"),
        }
        println!(
            "THROTTLE_CHILD_STATE:{}",
            observed_priority_state().expect("child priority query after syscall")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn lower_to_background_actually_raises_nice_on_linux() {
        const NAME: &str = "throttle::tests::lower_to_background_actually_raises_nice_on_linux";
        if run_priority_in_exact_child(NAME) {
            return;
        }
        test_child::stage("throttle:linux-before-syscall").expect("test-child stage must flush");
        println!(
            "THROTTLE_CHILD_BEFORE:{}",
            observed_priority_state().expect("child priority query before syscall")
        );
        // getpriority returns nice value (-20..19). After lower_to_background()
        // it must be >= the pre-call value (raised = lower priority).
        // Note: getpriority can legitimately return -1 as a nice value, so we
        // must clear errno first and check it.
        unsafe {
            *libc::__errno_location() = 0;
            let before = libc::getpriority(libc::PRIO_PROCESS, 0);
            assert_eq!(*libc::__errno_location(), 0, "getpriority before failed");

            match lower_to_background() {
                Ok(()) => println!("THROTTLE_CHILD_BRANCH:ok"),
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    println!("THROTTLE_CHILD_BRANCH:permission-denied");
                }
                Err(e) => panic!("unexpected error kind from lower_to_background: {e:?}"),
            }

            *libc::__errno_location() = 0;
            let after = libc::getpriority(libc::PRIO_PROCESS, 0);
            assert_eq!(*libc::__errno_location(), 0, "getpriority after failed");

            assert!(
                after >= before,
                "nice should have risen (lower priority); before={before} after={after}"
            );
        }
        println!(
            "THROTTLE_CHILD_STATE:{}",
            observed_priority_state().expect("child priority query after syscall")
        );
    }
}
