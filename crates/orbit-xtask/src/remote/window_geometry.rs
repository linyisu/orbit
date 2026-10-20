//! Native maximization acceptance against the isolated desktop process only.
use anyhow::{Result, ensure};
use std::time::{Duration, Instant};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, POINT, RECT},
    Graphics::Gdi::{
        ClientToScreen, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    },
    UI::WindowsAndMessaging::{
        EnumWindows, GetClientRect, GetWindowThreadProcessId, IsWindowVisible, IsZoomed,
        SW_MAXIMIZE, SW_RESTORE, ShowWindowAsync,
    },
};

struct Search {
    pid: u32,
    window: HWND,
}

unsafe extern "system" fn find(window: HWND, data: LPARAM) -> i32 {
    // SAFETY: EnumWindows synchronously borrows the Search passed by verify.
    let search = unsafe { &mut *(data as *mut Search) };
    let mut pid = 0;
    // SAFETY: EnumWindows supplied a live handle and pid is writable.
    unsafe { GetWindowThreadProcessId(window, &mut pid) };
    // SAFETY: The callback handle is valid for this enumeration.
    if pid == search.pid && unsafe { IsWindowVisible(window) } != 0 {
        search.window = window;
        return 0;
    }
    1
}

pub(super) fn verify(pid: u32) -> Result<()> {
    let mut search = Search {
        pid,
        window: std::ptr::null_mut(),
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while search.window.is_null() && Instant::now() < deadline {
        // SAFETY: Search lives until synchronous enumeration returns.
        unsafe { EnumWindows(Some(find), &mut search as *mut Search as LPARAM) };
        std::thread::sleep(Duration::from_millis(20));
    }
    ensure!(
        !search.window.is_null(),
        "isolated desktop window was not shown"
    );
    // SAFETY: This handle belongs to the isolated child identified above.
    unsafe { ShowWindowAsync(search.window, SW_MAXIMIZE) };
    let deadline = Instant::now() + Duration::from_secs(5);
    let result = loop {
        if let Ok(()) = check(search.window) {
            break Ok(());
        }
        if Instant::now() >= deadline {
            break check(search.window);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // SAFETY: Restore only the isolated child; never operate on other windows.
    unsafe { ShowWindowAsync(search.window, SW_RESTORE) };
    result?;
    println!("PASS: maximized client covers the monitor work area without a desktop gap");
    Ok(())
}

fn check(window: HWND) -> Result<()> {
    let mut client = RECT::default();
    let mut origin = POINT::default();
    // SAFETY: MONITORINFO is a plain Win32 structure with no invalid bit patterns.
    let mut monitor: MONITORINFO = unsafe { std::mem::zeroed() };
    monitor.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    // SAFETY: All outputs are writable and window belongs to the live child.
    unsafe {
        ensure!(IsZoomed(window) != 0, "desktop is not maximized");
        ensure!(
            GetClientRect(window, &mut client) != 0,
            "cannot measure client"
        );
        ensure!(
            ClientToScreen(window, &mut origin) != 0,
            "cannot locate client"
        );
        let display = MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST);
        ensure!(
            GetMonitorInfoW(display, &mut monitor) != 0,
            "cannot measure monitor"
        );
    }
    let edges = [
        origin.x - monitor.rcWork.left,
        origin.y - monitor.rcWork.top,
        origin.x + client.right - monitor.rcWork.right,
        origin.y + client.bottom - monitor.rcWork.bottom,
    ];
    ensure!(
        edges
            .iter()
            .all(|delta| delta.abs() <= 1),
        "maximized client/work-area edge differences: {edges:?}"
    );
    Ok(())
}
