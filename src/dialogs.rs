//! Native file dialogs through the platform's own helper programs
//! (`osascript`, `zenity`/`kdialog`, PowerShell), which avoids a GUI
//! toolkit dependency. Values are passed as arguments or environment
//! variables, never spliced into script text.

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn run(cmd: &mut Command) -> Option<PathBuf> {
    let out = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let line = text.trim_end_matches(['\r', '\n']);
    (!line.is_empty()).then(|| PathBuf::from(line))
}

/// Asks for an audio file to open.
pub fn open() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        run(Command::new("osascript").args(["-e", "POSIX path of (choose file with prompt \"Open an audio file\")"]))
    } else if cfg!(windows) {
        run(Command::new("powershell").args([
            "-NoProfile",
            "-Command",
            "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.OpenFileDialog; \
             $d.Title = 'Open an audio file'; if ($d.ShowDialog() -eq 'OK') { $d.FileName }",
        ]))
    } else {
        run(Command::new("zenity").args(["--file-selection", "--title=Open an audio file"]))
            .or_else(|| run(Command::new("kdialog").args(["--title", "Open an audio file", "--getopenfilename", "."])))
    }
}

/// Asks where to save the image, suggesting `name`.
pub fn save(name: &str) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        run(Command::new("osascript")
            .args([
                "-e",
                "on run argv",
                "-e",
                "POSIX path of (choose file name with prompt \"Save the spectrogram\" default name (item 1 of argv))",
                "-e",
                "end run",
            ])
            .arg(name))
    } else if cfg!(windows) {
        run(Command::new("powershell").env("SPECTRO_NAME", name).args([
            "-NoProfile",
            "-Command",
            "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.SaveFileDialog; \
                 $d.Title = 'Save the spectrogram'; $d.Filter = 'PNG image|*.png'; $d.FileName = $env:SPECTRO_NAME; \
                 if ($d.ShowDialog() -eq 'OK') { $d.FileName }",
        ]))
    } else {
        run(Command::new("zenity")
            .args(["--file-selection", "--save", "--confirm-overwrite", "--title=Save the spectrogram"])
            .arg(format!("--filename={name}")))
        .or_else(|| {
            run(Command::new("kdialog").args(["--title", "Save the spectrogram", "--getsavefilename"]).arg(name))
        })
    }
}

/// Shows `path` in the file manager.
pub fn reveal(path: &std::path::Path) {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg("-R").arg(path);
        c
    } else if cfg!(windows) {
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path.as_os_str());
        let mut c = Command::new("explorer");
        c.arg(arg);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(path.parent().unwrap_or(path));
        c
    };
    let _ = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}
