// "Start with Windows": a per-user Task Scheduler task that runs betterUI the
// moment you sign in. Windows holds back apps in the Run registry key for
// several seconds after sign-in, so with the Run key the Windows taskbar
// showed first and tray apps that started meanwhile registered their icons
// with Explorer instead of betterUI. A task for your own account needs no
// admin rights. If it can't be created, the Run key is used instead.

use std::os::windows::process::CommandExt;
use std::process::Command;

use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE: PCWSTR = w!("betterUI");
const TASK: &str = "betterUI";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The command line Windows runs at sign-in, if betterUI is registered.
fn run_key_command() -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if rc.is_err() {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn current_command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\"", exe.display()))
}

pub fn is_enabled() -> bool {
    task_command().is_some() || run_key_command().is_some()
}

pub fn set_enabled(on: bool) {
    if on {
        let Some(cmd) = current_command() else { return };
        if create_task(&cmd) {
            remove_run_key();
        } else {
            crate::log::write("could not create the sign-in task; using the Run key instead");
            set_run_key(&cmd);
        }
    } else {
        if task_command().is_some() {
            let _ = schtasks(&["/Delete", "/TN", TASK, "/F"]);
        }
        remove_run_key();
    }
}

/// If start-with-Windows is on but points at a different copy of the exe
/// (e.g. a newer release downloaded to another folder), point it at this
/// one. A setting from an older betterUI (Run key) moves to the task.
pub fn refresh_path() {
    let Some(current) = current_command() else { return };
    let current_exe = current.trim_matches('"').to_lowercase();
    if let Some(task) = task_command() {
        if task.to_lowercase() != current_exe {
            crate::log::write(&format!("start with Windows now points to {current}"));
            set_enabled(true);
        }
    } else if run_key_command().is_some() {
        crate::log::write("start with Windows: moving from the Run key to a sign-in task");
        set_enabled(true);
    }
}

fn schtasks(args: &[&str]) -> Option<String> {
    let out = Command::new("schtasks.exe").args(args).creation_flags(CREATE_NO_WINDOW).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        crate::log::write(&format!(
            "schtasks {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
        None
    }
}

/// The exe the sign-in task runs, if the task exists.
fn task_command() -> Option<String> {
    let out = Command::new("schtasks.exe")
        .args(["/Query", "/TN", TASK, "/XML"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let xml = String::from_utf8_lossy(&out.stdout).to_string();
    let start = xml.find("<Command>")? + "<Command>".len();
    let end = xml[start..].find("</Command>")? + start;
    Some(xml_unescape(xml[start..end].trim()).trim_matches('"').to_string())
}

fn create_task(cmd: &str) -> bool {
    let exe = cmd.trim_matches('"');
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(d), Ok(u)) => format!("{d}\\{u}"),
        (_, Ok(u)) => u,
        _ => return false,
    };
    let xml = task_xml(&xml_escape(&user), &xml_escape(exe));
    let path = std::env::temp_dir().join("betterui-task.xml");
    // schtasks reads UTF-16 task files reliably: little-endian with a BOM.
    let mut bytes = vec![0xFF, 0xFE];
    for u in xml.encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    if std::fs::write(&path, bytes).is_err() {
        return false;
    }
    let ok = schtasks(&["/Create", "/TN", TASK, "/XML", &path.to_string_lossy(), "/F"]).is_some();
    let _ = std::fs::remove_file(&path);
    ok
}

/// Runs at sign-in of this user only, without admin rights, at normal
/// priority (tasks default to below normal), with no time limit (tasks
/// default to being stopped after 3 days), on battery too.
fn task_xml(user: &str, exe: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Starts betterUI when you sign in. Turn it off from betterUI: right-click the bar, Start with Windows.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>"{exe}"</Command>
    </Exec>
  </Actions>
</Task>
"#
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

fn set_run_key(cmd: &str) {
    let wide: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
    let rc = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE,
            REG_SZ.0,
            Some(wide.as_ptr() as *const _),
            (wide.len() * 2) as u32,
        )
    };
    if rc.is_err() {
        crate::log::write(&format!("could not enable start with Windows: {rc:?}"));
    }
}

fn remove_run_key() {
    if run_key_command().is_some() {
        let _ = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_xml_round_trips_the_exe_path() {
        let exe = r"C:\Users\A & B\Downloads\betterui.exe";
        let xml = task_xml("PC\\a", &xml_escape(exe));
        let start = xml.find("<Command>").unwrap() + "<Command>".len();
        let end = xml.find("</Command>").unwrap();
        assert_eq!(xml_unescape(&xml[start..end]).trim_matches('"'), exe);
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
    }
}
