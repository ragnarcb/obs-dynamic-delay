//! Windows resources: icon, version information (shown in the file's
//! Properties > Details, read by antivirus reputation systems) and manifest.

fn main() {
    println!("cargo:rerun-if-changed=installer/art/app.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = env!("CARGO_PKG_VERSION");
    let mut res = winresource::WindowsResource::new();
    res.set_icon("installer/art/app.ico")
        .set("ProductName", "Dynamic Delay for OBS")
        .set("FileDescription", "Dynamic Delay for OBS (relay and setup helper)")
        .set("CompanyName", "ragnarcb")
        .set("LegalCopyright", "Copyright (c) 2026 ragnarcb. MIT + Commons Clause.")
        .set("OriginalFilename", "obs-dynamic-delay.exe")
        .set("InternalName", "obs-dynamic-delay")
        .set("Comments", "https://github.com/ragnarcb/obs-dynamic-delay")
        .set("ProductVersion", version)
        .set("FileVersion", version)
        .set_manifest(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="ragnarcb.ObsDynamicDelay" version="1.0.0.0" processorArchitecture="amd64"/>
  <description>Dynamic Delay for OBS</description>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security><requestedPrivileges><requestedExecutionLevel level="asInvoker" uiAccess="false"/></requestedPrivileges></security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application><supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/></application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </windowsSettings>
  </application>
</assembly>"#,
        );
    // numeric version for the VERSIONINFO header: 0.8.1 -> 0.8.1.0
    let mut n = version.split('.').map(|x| x.parse::<u64>().unwrap_or(0)).chain(std::iter::repeat(0));
    let v = (n.next().unwrap() << 48) | (n.next().unwrap() << 32) | (n.next().unwrap() << 16) | n.next().unwrap();
    res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, v);
    res.set_version_info(winresource::VersionInfo::FILEVERSION, v);
    if let Err(e) = res.compile() {
        panic!("Windows resources: {e}");
    }
}
