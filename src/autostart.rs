use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
};

const RUN_KEY_PATH: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const VALUE_NAME: PCWSTR = w!("VoxDrop");

/// Returns whether VoxDrop is currently registered to auto-start on Windows sign-in.
pub fn is_auto_start_enabled() -> bool {
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            RUN_KEY_PATH,
            Some(0),
            KEY_QUERY_VALUE,
            &mut hkey,
        ) != ERROR_SUCCESS
        {
            return false;
        }
        let mut size: u32 = 0;
        let exists =
            RegQueryValueExW(hkey, VALUE_NAME, None, None, None, Some(&mut size)) == ERROR_SUCCESS
                && size > 0;
        let _ = RegCloseKey(hkey);
        exists
    }
}

/// Enables or disables Windows startup registration for the current `voxdrop.exe`.
pub fn set_auto_start(enabled: bool) -> Result<(), String> {
    unsafe {
        let mut hkey = HKEY::default();
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            RUN_KEY_PATH,
            Some(0),
            KEY_SET_VALUE | KEY_QUERY_VALUE,
            &mut hkey,
        )
        .ok()
        .map_err(|e| format!("レジストリのオープンに失敗しました: {}", e))?;

        let result = if enabled {
            let exe = std::env::current_exe()
                .map_err(|e| format!("実行ファイルパスの取得に失敗しました: {}", e))?;
            let cmd = format!("\"{}\"", exe.display());
            let wide: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
            RegSetValueExW(hkey, VALUE_NAME, Some(0), REG_SZ, Some(bytes))
                .ok()
                .map_err(|e| format!("自動起動の登録に失敗しました: {}", e))
        } else {
            let err = RegDeleteValueW(hkey, VALUE_NAME);
            if err == ERROR_SUCCESS || err == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                Err(format!("自動起動の解除に失敗しました: {:?}", err))
            }
        };

        let _ = RegCloseKey(hkey);
        result
    }
}
