use std::io::{self, Write};

#[cfg(not(windows))]
pub(crate) fn query_host_terminal_appearance() {
    let _ = write_host_terminal_appearance_query(io::stdout());
}

#[cfg(any(not(windows), test))]
pub(crate) fn write_host_terminal_appearance_query(mut writer: impl Write) -> io::Result<()> {
    writer.write_all(crate::terminal_theme::HOST_COLOR_SCHEME_QUERY_SEQUENCE.as_bytes())?;
    writer.flush()
}

pub(crate) fn query_host_terminal_theme() {
    let _ = write_host_terminal_theme_query(io::stdout());
}

pub(crate) fn write_host_terminal_theme_query(mut writer: impl Write) -> io::Result<()> {
    let query = crate::terminal_theme::host_terminal_theme_query_sequence(
        crate::platform::should_query_host_terminal_palette(),
    );
    writer.write_all(query.as_bytes())?;
    writer.flush()
}

pub(crate) fn query_host_cell_size() {
    let _ = write_host_cell_size_query(io::stdout());
}

pub(crate) fn write_host_cell_size_query(mut writer: impl Write) -> io::Result<()> {
    writer.write_all(b"\x1b[16t")?;
    writer.flush()
}
