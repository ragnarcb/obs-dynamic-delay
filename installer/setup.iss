; Dynamic Delay for OBS: Windows Setup (Inno Setup 6).
;
; Build:  ISCC /DAppVersion=0.9.0 installer\setup.iss
; Sign:   ISCC /DAppVersion=0.9.0 /DSign "/Ssigntool=signtool.exe sign /fd sha256 /tr http://timestamp.digicert.com /td sha256 /f cert.pfx /p PASS $f" installer\setup.iss
;
; The Setup copies the program to %APPDATA%\obs-dynamic-delay and then runs
; "obs-dynamic-delay.exe --install --quiet", which adds the script and the
; panel to OBS and points OBS at the relay (the same steps as before).

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef Exe
  #define Exe "..\target\release\obs-dynamic-delay.exe"
#endif
#define AppName "Dynamic Delay for OBS"
; TestRoot: build a Setup that installs into a test folder (used by the automated test)
#ifdef TestRoot
  #define AppIdValue "{{2A0B1C3D-TEST-4E5F-8A9B-0C1D2E3F4A5B}"
#else
  #define AppIdValue "{{8C1B2E54-5B7A-4F7E-9C1D-3D6A9E2F7B41}"
#endif
#define Repo "https://github.com/ragnarcb/obs-dynamic-delay"

[Setup]
AppId={#AppIdValue}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=ragnarcb
AppPublisherURL=https://github.com/ragnarcb
AppSupportURL={#Repo}/issues
AppUpdatesURL={#Repo}/releases
AppCopyright=Copyright (c) 2026 ragnarcb
VersionInfoVersion={#AppVersion}
VersionInfoCompany=ragnarcb
VersionInfoDescription={#AppName} Setup
VersionInfoProductName={#AppName}
; per-user install, no administrator prompt: OBS reads its scripts from the user profile
PrivilegesRequired=lowest
#ifdef TestRoot
DefaultDirName={#TestRoot}\obs-dynamic-delay
#else
DefaultDirName={userappdata}\obs-dynamic-delay
#endif
DisableDirPage=yes
DisableProgramGroupPage=yes
DefaultGroupName={#AppName}
UninstallDisplayName={#AppName}
UninstallDisplayIcon={app}\obs-dynamic-delay.exe
OutputDir=..\dist
OutputBaseFilename=Dynamic-Delay-Setup
SetupIconFile=art\app.ico
WizardStyle=modern
DisableWelcomePage=no
WizardSizePercent=100
WizardImageFile=art\wizard-100.bmp,art\wizard-125.bmp,art\wizard-150.bmp,art\wizard-200.bmp,art\wizard-250.bmp
WizardSmallImageFile=art\small-100.bmp,art\small-125.bmp,art\small-150.bmp,art\small-200.bmp,art\small-250.bmp
LicenseFile=license.txt
ShowLanguageDialog=yes
UsePreviousLanguage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
Compression=lzma2/ultra64
SolidCompression=yes
CloseApplications=yes
RestartApplications=no
SetupLogging=yes
#ifdef Sign
SignTool=signtool
SignedUninstaller=yes
#endif

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "pt"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"
Name: "es"; MessagesFile: "compiler:Languages\Spanish.isl"

[Messages]
en.WelcomeLabel2=This installs [name/ver] on your computer.%n%nTurn your stream delay on and off at any moment, while you are live, and get delete before it airs, instant replay, clips, multistream and more, all in a panel inside OBS.%n%nThe Setup adds the script and the Dynamic Delay panel to OBS and makes OBS stream through the delay. Your current stream settings are saved and come back if you uninstall.
pt.WelcomeLabel2=Isto instala o [name/ver] no seu computador.%n%nLigue e desligue o delay da sua live a qualquer momento, com a transmissão no ar, e tenha apagar antes de ir ao ar, replay instantâneo, clipes, multistream e mais, tudo num painel dentro do OBS.%n%nO instalador adiciona o script e o painel Delay dinâmico no OBS e faz o OBS transmitir pelo delay. Sua configuração de transmissão atual fica salva e volta se você desinstalar.
es.WelcomeLabel2=Esto instala [name/ver] en tu computadora.%n%nActiva y desactiva el delay de tu transmisión en cualquier momento, en pleno directo, y ten borrar antes de salir al aire, repetición instantánea, clips, multistream y más, todo en un panel dentro de OBS.%n%nEl instalador agrega el script y el panel Delay dinámico a OBS y hace que OBS transmita a través del delay. Tu configuración de transmisión actual se guarda y vuelve si desinstalas.
en.FinishedLabel=[name] is installed. Open OBS: the panel is under Docks > Dynamic Delay. Paste your stream key in its Settings if it was not imported from OBS.
pt.FinishedLabel=O [name] está instalado. Abra o OBS: o painel fica em Docks > Delay dinâmico. Cole sua chave de transmissão em Configuração se ela não veio do OBS.
es.FinishedLabel=[name] está instalado. Abre OBS: el panel está en Docks > Delay dinámico. Pega tu clave de transmisión en su Configuración si no se importó de OBS.

en.ClickFinish=
pt.ClickFinish=
es.ClickFinish=

[CustomMessages]
en.Configuring=Setting up OBS (script, panel and stream settings)...
pt.Configuring=Configurando o OBS (script, painel e transmissão)...
es.Configuring=Configurando OBS (script, panel y transmisión)...
en.OpenObs=Open OBS now
pt.OpenObs=Abrir o OBS agora
es.OpenObs=Abrir OBS ahora
en.CloseObs=OBS is open.%n%nClose OBS to continue (it rewrites its settings when it closes), then click Retry.
pt.CloseObs=O OBS está aberto.%n%nFeche o OBS para continuar (ele regrava as configurações ao fechar) e clique em Repetir.
es.CloseObs=OBS está abierto.%n%nCierra OBS para continuar (reescribe su configuración al cerrarse) y luego haz clic en Reintentar.
en.ObsStillOpen=OBS is still open. Close it and run the Setup again.
pt.ObsStillOpen=O OBS ainda está aberto. Feche e rode o instalador de novo.
es.ObsStillOpen=OBS sigue abierto. Ciérralo y vuelve a ejecutar el instalador.
en.NoObs=OBS Studio settings were not found.%n%nInstall OBS Studio (obsproject.com) and open it at least once, then run this Setup again.
pt.NoObs=Não achei a configuração do OBS Studio.%n%nInstale o OBS Studio (obsproject.com) e abra ele pelo menos uma vez, depois rode este instalador de novo.
es.NoObs=No se encontró la configuración de OBS Studio.%n%nInstala OBS Studio (obsproject.com) y ábrelo al menos una vez, luego vuelve a ejecutar este instalador.
en.ConfigFailed=The files were installed, but setting up OBS failed:%n%n%1%n%nRun the Setup again, or see {#Repo}#troubleshooting
pt.ConfigFailed=Os arquivos foram instalados, mas a configuração do OBS falhou:%n%n%1%n%nRode o instalador de novo, ou veja {#Repo}/blob/main/README.pt-BR.md
es.ConfigFailed=Los archivos se instalaron, pero la configuración de OBS falló:%n%n%1%n%nVuelve a ejecutar el instalador, o consulta {#Repo}/blob/main/README.es.md
en.RestoreFailed=Uninstall stopped because OBS could not be restored (code %1).%n%nThe program and recovery files have been kept. Close OBS and try again. Details: %2
pt.RestoreFailed=A desinstalação parou porque não foi possível restaurar o OBS (código %1).%n%nO programa e os arquivos de recuperação foram mantidos. Feche o OBS e tente de novo. Detalhes: %2
es.RestoreFailed=La desinstalación se detuvo porque no se pudo restaurar OBS (código %1).%n%nSe conservaron el programa y los archivos de recuperación. Cierra OBS e inténtalo de nuevo. Detalles: %2
en.Done=Done in OBS:
pt.Done=Feito no OBS:
es.Done=Hecho en OBS:
en.ForceUninstall=OBS recovery failed. Uninstall anyway and keep the recovery files?%n%nOBS may still point to the removed relay. Keep the .dd-backup/.dd-changes.json files in your OBS profiles and the application folder for manual recovery. Choose No to fix the problem and retry.
pt.ForceUninstall=A restauração do OBS falhou. Desinstalar mesmo assim e manter os arquivos de recuperação?%n%nO OBS pode continuar apontando para o relay removido. Guarde os arquivos .dd-backup/.dd-changes.json nos perfis do OBS e a pasta do aplicativo para recuperação manual. Escolha Não para corrigir o problema e tentar novamente.
es.ForceUninstall=La restauración de OBS falló. ¿Desinstalar de todos modos y conservar los archivos de recuperación?%n%nOBS puede seguir apuntando al relay eliminado. Conserva los archivos .dd-backup/.dd-changes.json en los perfiles de OBS y la carpeta de la aplicación para recuperación manual. Elige No para corregir el problema y reintentar.
en.DeleteSettings=Also delete your Dynamic Delay settings (stream keys, access token, clips folder choice)?%n%nChoose No to keep them for a future install.
pt.DeleteSettings=Apagar também a configuração do Delay dinâmico (chaves de transmissão, token de acesso, pasta dos clipes)?%n%nEscolha Não para manter para uma próxima instalação.
es.DeleteSettings=¿Borrar también tu configuración del Delay dinámico (claves de transmisión, token de acceso, carpeta de clips)?%n%nElige No para conservarla para una próxima instalación.
en.GitHub=Dynamic Delay on GitHub
pt.GitHub=Delay dinâmico no GitHub
es.GitHub=Delay dinámico en GitHub
en.Guide=How to use
pt.Guide=Como usar
es.Guide=Cómo usar

[Files]
Source: "{#Exe}"; DestDir: "{app}"; DestName: "obs-dynamic-delay.exe"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}\{cm:Guide}"; Filename: "{#Repo}#readme"
Name: "{autoprograms}\{#AppName}\{cm:GitHub}"; Filename: "{#Repo}"
Name: "{autoprograms}\{#AppName}\{cm:UninstallProgram,{#AppName}}"; Filename: "{uninstallexe}"

[Run]
#ifdef TestRoot
Filename: "{app}\obs-dynamic-delay.exe"; Parameters: "--launch-obs"; Description: "{cm:OpenObs}"; Flags: postinstall nowait skipifsilent unchecked
#else
Filename: "{app}\obs-dynamic-delay.exe"; Parameters: "--launch-obs"; Description: "{cm:OpenObs}"; Flags: postinstall nowait skipifsilent
#endif

[UninstallDelete]
Type: files; Name: "{app}\obs-dynamic-delay.lua"
Type: files; Name: "{app}\dock.html"
Type: files; Name: "{app}\obs-dynamic-delay.log"
Type: files; Name: "{app}\obs-dynamic-delay-old.exe"

[Code]
var
  SetupSteps: String;
  KeepRecovery: Boolean;


function ObsConfigDir: String;
begin
#ifdef TestRoot
  Result := '{#TestRoot}\obs-studio';
#else
  Result := ExpandConstant('{userappdata}\obs-studio');
#endif
end;

function ObsRunning: Boolean;
var
  Locator, Service, Found: Variant;
begin
  Result := False;
  try
    Locator := CreateOleObject('WbemScripting.SWbemLocator');
    Service := Locator.ConnectServer('.', 'root\CIMV2');
    Found := Service.ExecQuery('SELECT ProcessId FROM Win32_Process WHERE Name = ''obs64.exe''');
    Result := Found.Count > 0;
  except
  end;
end;

function InitializeSetup: Boolean;
begin
  Result := True;
  if not DirExists(ObsConfigDir + '\basic') then
  begin
    SuppressibleMsgBox(CustomMessage('NoObs'), mbError, MB_OK, IDOK);
    Result := False;
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  while ObsRunning do
  begin
    if SuppressibleMsgBox(CustomMessage('CloseObs'), mbError, MB_RETRYCANCEL, IDCANCEL) = IDCANCEL then
    begin
      Result := CustomMessage('ObsStillOpen');
      exit;
    end;
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Code: Integer;
  Log: AnsiString;
begin
  if CurStep = ssPostInstall then
  begin
    WizardForm.StatusLabel.Caption := CustomMessage('Configuring');
    WizardForm.ProgressGauge.Style := npbstMarquee;
    Exec(ExpandConstant('{app}\obs-dynamic-delay.exe'), '--install --quiet --lang ' + ActiveLanguage, ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, Code);
    WizardForm.ProgressGauge.Style := npbstNormal;
    Log := '';
    LoadStringFromFile(ExpandConstant('{app}\setup.log'), Log);
    Log := UTF8Decode(Log);
    if Code <> 0 then
      SuppressibleMsgBox(FmtMessage(CustomMessage('ConfigFailed'), [String(Log)]), mbError, MB_OK, IDOK)
    else
      SetupSteps := String(Log);
  end;
end;

procedure CurPageChanged(CurPageID: Integer);
var
  Lines: TStringList;
  StepsLabel: TNewStaticText;
  I: Integer;
  Text: String;
begin
  if (CurPageID = wpFinished) and (SetupSteps <> '') then
  begin
    Lines := TStringList.Create;
    Lines.Text := Trim(SetupSteps);
    // the install folder and the local relay address mean little to a streamer
    for I := Lines.Count - 1 downto 0 do
      if (I = 0) or (Pos('127.0.0.1', Lines[I]) > 0) then
        Lines.Delete(I);
    WizardForm.FinishedLabel.AdjustHeight;
    // the Open OBS checkbox right under the message (the list fills the page by default)
    WizardForm.RunList.Top := WizardForm.FinishedLabel.Top + WizardForm.FinishedLabel.Height + ScaleY(8);
    WizardForm.RunList.Height := ScaleY(22) * WizardForm.RunList.Items.Count;
    StepsLabel := TNewStaticText.Create(WizardForm);
    StepsLabel.Parent := WizardForm.FinishedPage;
    StepsLabel.Left := WizardForm.FinishedLabel.Left;
    StepsLabel.Top := WizardForm.RunList.Top + WizardForm.RunList.Height + ScaleY(10);
    StepsLabel.Width := WizardForm.FinishedLabel.Width;
    StepsLabel.AutoSize := False;
    StepsLabel.WordWrap := True;
    StepsLabel.Font.Size := 8;
    StepsLabel.Font.Color := $505050;
    // drop the oldest steps until the list fits the page (the last ones matter most)
    repeat
      Text := CustomMessage('Done');
      for I := 0 to Lines.Count - 1 do
        Text := Text + #13#10 + '- ' + Lines[I];
      StepsLabel.Caption := Text;
      StepsLabel.AdjustHeight;
      if (StepsLabel.Top + StepsLabel.Height <= WizardForm.FinishedPage.ClientHeight) or (Lines.Count = 0) then
        break;
      Lines.Delete(0);
    until False;
    StepsLabel.Visible := Lines.Count > 0;
    Lines.Free;
    SetupSteps := '';
  end;
end;

// Older installed versions may have unconditional deletion records in their
// appended uninstall log. Save legacy recovery evidence outside {app} first.
procedure PreserveRecoveryFile(Name: String);
var
  Source, DestDir: String;
begin
  Source := ExpandConstant('{app}\') + Name;
  if FileExists(Source) then
  begin
    DestDir := ObsConfigDir + '\dynamic-delay-recovery';
    if not ForceDirectories(DestDir) then
      RaiseException('Cannot preserve recovery files in ' + DestDir);
    if not FileCopy(Source, DestDir + '\' + Name, False) then
      RaiseException('Cannot preserve recovery file: ' + Source);
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Code: Integer;
begin
  if CurUninstallStep = usUninstall then
  begin
    // This event runs after confirmation but before ANY files are deleted.
    // An exception here aborts Inno's uninstall, unlike an [UninstallRun] exit code.
    while ObsRunning do
      if SuppressibleMsgBox(CustomMessage('CloseObs'), mbError, MB_RETRYCANCEL, IDCANCEL) = IDCANCEL then
        RaiseException(CustomMessage('ObsStillOpen'));
    Code := -1;
    if not Exec(ExpandConstant('{app}\obs-dynamic-delay.exe'),
      '--uninstall --quiet --lang ' + ActiveLanguage, ExpandConstant('{app}'),
      SW_HIDE, ewWaitUntilTerminated, Code) then
      Code := -1;
    if Code <> 0 then
    begin
      if SuppressibleMsgBox(CustomMessage('ForceUninstall'), mbConfirmation,
        MB_YESNO or MB_DEFBUTTON2, IDNO) <> IDYES then
        RaiseException(FmtMessage(CustomMessage('RestoreFailed'), [IntToStr(Code),
          ExpandConstant('{app}\setup.log')]));
      if ObsRunning then RaiseException(CustomMessage('ObsStillOpen'));
      KeepRecovery := True;
      // Missing/broken helper must not make removal impossible after consent.
      // Inno also preserves legacy recovery data when the helper cannot run.
      Exec(ExpandConstant('{app}\obs-dynamic-delay.exe'),
        '--uninstall --quiet --force --lang ' + ActiveLanguage, ExpandConstant('{app}'),
        SW_HIDE, ewWaitUntilTerminated, Code);
      PreserveRecoveryFile('obs-service-backup.json');
      PreserveRecoveryFile('config.toml');
      PreserveRecoveryFile('setup.log');
    end;
  end;
  if (CurUninstallStep = usPostUninstall) and not KeepRecovery then
  begin
    DeleteFile(ExpandConstant('{app}\setup.log'));
    DeleteFile(ExpandConstant('{app}\obs-service-backup.json'));
  end;
  if (CurUninstallStep = usPostUninstall) and not UninstallSilent and not KeepRecovery then
    if MsgBox(CustomMessage('DeleteSettings'), mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
      DelTree(ExpandConstant('{app}'), True, True, True);
end;
