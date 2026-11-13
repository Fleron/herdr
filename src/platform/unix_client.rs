pub(crate) fn terminal_title_for_presentation(title: &str) -> &str {
    title
}

#[cfg(unix)]
pub(crate) fn read_fd(fd: std::os::fd::RawFd, data: &mut [u8]) -> std::io::Result<usize> {
    let result = unsafe { libc::read(fd, data.as_mut_ptr().cast(), data.len()) };
    if result < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(result as usize)
    }
}

#[cfg(unix)]
pub(crate) fn poll_fd_readable(fd: std::os::fd::RawFd, timeout_ms: i32) -> std::io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
    if result < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(result > 0)
    }
}
#[cfg(unix)]
/// The tty's erase character (VERASE) on stdin, when one is set.
pub(crate) fn terminal_erase_byte() -> Option<u8> {
    let mut termios: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut termios) } != 0 {
        return None;
    }
    let erase = termios.c_cc[libc::VERASE];
    // 0 (Linux) and 0xff (BSD) disable the special character.
    (erase != 0 && erase != 0xff).then_some(erase)
}
