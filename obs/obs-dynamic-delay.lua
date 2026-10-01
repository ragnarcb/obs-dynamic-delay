-- obs-dynamic-delay: connects OBS to the dynamic delay relay.
-- Installed by obs-dynamic-delay.exe (or: Tools > Scripts > "+", with the exe in the same folder).
--
-- * starts the relay hidden together with OBS and closes it with OBS
-- * hotkeys (Settings > Hotkeys > "Dynamic Delay")
-- * runs inside OBS what the panel asks for (configure/restore the stream settings)
-- * switches to the delay scene while the delay builds up (grow mode "scene")
-- * panic button: cover scene and mute; tells the relay which scene is on air (scene rules)
-- * phone deck: switch scenes, toggle mute, start/stop streaming and recording
-- * keeps the platform's encoder rules (bitrate cap, 2 s keyframes) while OBS streams to the relay
--
-- Texts are English, Portuguese or Spanish, following `language` in config.toml.

obs = obslua
local ffi = require("ffi")

local is_windows = ffi.os == "Windows"
local EXE_NAME = is_windows and "obs-dynamic-delay.exe" or "obs-dynamic-delay"

local S = nil
local manage_relay = true
local relay_path = ""
local step = 5
local ports = { rtmp = 1935, http = 8787, udp = 8788 }
local lang = "en"
local api_token = ""
local dest_urls = {}

-- Picks the text for the configured language (Spanish falls back to English).
local function L(en, pt, es)
  if lang == "pt" then return pt end
  if lang == "es" and es then return es end
  return en
end

---------------------------------------------------------------------------
-- Sockets (LuaJIT FFI, no extra dependencies)
---------------------------------------------------------------------------
local sock_lib, shell32 = nil, nil

ffi.cdef [[
  uint16_t htons(uint16_t v);
  uint32_t inet_addr(const char *cp);
]]

if is_windows then
  ffi.cdef [[
    typedef uintptr_t SOCKET;
    typedef struct { uint16_t family; uint16_t port; uint32_t addr; char zero[8]; } dd_sockaddr_in;
    int WSAStartup(uint16_t version, void *data);
    SOCKET socket(int af, int type, int protocol);
    int sendto(SOCKET s, const char *buf, int len, int flags, const void *to, int tolen);
    int recvfrom(SOCKET s, char *buf, int len, int flags, void *from, int *fromlen);
    int setsockopt(SOCKET s, int level, int optname, const void *optval, int optlen);
    int closesocket(SOCKET s);
    int WSAIoctl(SOCKET s, uint32_t code, void *in, uint32_t inlen, void *out, uint32_t outlen, uint32_t *ret, void *ov, void *cb);
    void *ShellExecuteA(void *hwnd, const char *op, const char *file, const char *params, const char *dir, int show);
  ]]
  sock_lib = ffi.load("ws2_32")
  shell32 = ffi.load("shell32")
  sock_lib.WSAStartup(0x0202, ffi.new("uint8_t[512]"))
else
  if ffi.os == "OSX" then
    ffi.cdef [[ typedef struct { uint8_t len; uint8_t family; uint16_t port; uint32_t addr; char zero[8]; } dd_sockaddr_in; ]]
  else
    ffi.cdef [[ typedef struct { uint16_t family; uint16_t port; uint32_t addr; char zero[8]; } dd_sockaddr_in; ]]
  end
  ffi.cdef [[
    typedef struct { int64_t sec; int64_t usec; } dd_timeval;
    int socket(int af, int type, int protocol);
    intptr_t sendto(int s, const void *buf, size_t len, int flags, const void *to, uint32_t tolen);
    intptr_t recvfrom(int s, void *buf, size_t len, int flags, void *from, uint32_t *fromlen);
    int setsockopt(int s, int level, int optname, const void *optval, uint32_t optlen);
    int close(int s);
  ]]
  sock_lib = ffi.C
end

local AF_INET, SOCK_DGRAM, IPPROTO_UDP = 2, 2, 17

local function new_sock()
  local s = sock_lib.socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP)
  if is_windows then
    -- SIO_UDP_CONNRESET off: an ICMP "port unreachable" must not break later reads
    sock_lib.WSAIoctl(s, 0x9800000C, ffi.new("uint32_t[1]", 0), 4, nil, 0, ffi.new("uint32_t[1]"), nil, nil)
  end
  return s
end

local cmd_sock, poll_sock = new_sock(), new_sock()
local buf = ffi.new("char[4096]")

local function set_timeout(s, ms)
  if is_windows then
    sock_lib.setsockopt(s, 0xffff, 0x1006, ffi.new("uint32_t[1]", ms), 4)
  else
    local tv = ffi.new("dd_timeval")
    tv.sec = math.floor(ms / 1000)
    tv.usec = (ms % 1000) * 1000
    local level, opt = 1, 20 -- Linux SOL_SOCKET, SO_RCVTIMEO
    if ffi.os == "OSX" then level, opt = 0xffff, 0x1006 end
    sock_lib.setsockopt(s, level, opt, tv, ffi.sizeof(tv))
  end
end

local function send_on(s, msg)
  local addr = ffi.new("dd_sockaddr_in")
  if ffi.os == "OSX" then addr.len = ffi.sizeof(addr) end
  addr.family = AF_INET
  addr.port = sock_lib.htons(ports.udp)
  addr.addr = sock_lib.inet_addr("127.0.0.1")
  return tonumber(sock_lib.sendto(s, msg, #msg, 0, addr, ffi.sizeof(addr))) >= 0
end

local function send(msg)
  return send_on(cmd_sock, msg)
end

local function recv_on(s, timeout_ms)
  set_timeout(s, timeout_ms)
  local n = tonumber(sock_lib.recvfrom(s, buf, 4096, 0, nil, nil))
  if n == nil or n <= 0 then return nil end
  return ffi.string(buf, n)
end

-- Sends a command and waits for the reply. Returns nil on timeout.
local function request(msg, timeout_ms)
  while recv_on(cmd_sock, 1) do end -- drop stale replies
  if not send(msg) then return nil end
  return recv_on(cmd_sock, timeout_ms)
end

local function relay_alive()
  return request("status", 250) ~= nil
end

---------------------------------------------------------------------------
-- Files and processes
---------------------------------------------------------------------------
local function dir() return script_path() end
local function config_path() return dir() .. "config.toml" end

local function exists(path)
  local f = io.open(path, "rb")
  if f then f:close() return true end
  return false
end

local function exe_path()
  if relay_path ~= "" then return relay_path end
  return dir() .. EXE_NAME
end

-- Ports and language come from the relay's config.toml (edited by the installer/panel).
local function read_ports()
  local f = io.open(config_path(), "r")
  if not f then return end
  local text = "\n" .. f:read("*a") -- so a key on the first line matches too
  f:close()
  local function port(key)
    return tonumber(text:match("\n%s*" .. key .. "%s*=%s*\"[^\"]*:(%d+)\""))
  end
  ports.rtmp = port("listen") or ports.rtmp
  ports.http = port("http_listen") or ports.http
  ports.udp = port("udp_listen") or ports.udp
  lang = text:match("\n%s*language%s*=%s*\"(%a+)\"") or lang
  api_token = text:match("\n%s*api_token%s*=%s*\"(%w+)\"") or api_token
  -- main destination plus every multistream destination
  dest_urls = {}
  for u in text:gmatch("\n%s*upstream_url%s*=%s*\"([^\"]*)\"") do table.insert(dest_urls, u) end
  for u in text:gmatch("\n%s*url%s*=%s*\"([^\"]*)\"") do table.insert(dest_urls, u) end
end

local function relay_server()
  return "rtmp://127.0.0.1:" .. ports.rtmp .. "/live"
end

local function open_target(target)
  if is_windows then
    shell32.ShellExecuteA(nil, "open", target, nil, nil, 1)
  elseif ffi.os == "OSX" then
    os.execute('open "' .. target .. '"')
  else
    os.execute('xdg-open "' .. target .. '" &')
  end
end

local function launch()
  local exe = exe_path()
  if not exists(exe) then
    obs.script_log(obs.LOG_WARNING, L("relay executable not found: ", "executável do relay não encontrado: ", "no se encontró el ejecutable del relay: ") .. exe)
    return false
  end
  if is_windows then
    local wd = exe:match("^(.*)[\\/]")
    -- 0 = SW_HIDE: the relay runs in the background, logs go to obs-dynamic-delay.log
    local r = shell32.ShellExecuteA(nil, "open", exe, '"' .. config_path() .. '"', wd, 0)
    if tonumber(ffi.cast("intptr_t", r)) <= 32 then
      obs.script_log(obs.LOG_WARNING, L("could not start ", "falha ao iniciar ", "no se pudo iniciar ") .. exe)
      return false
    end
  else
    os.execute('"' .. exe .. '" "' .. config_path() .. '" > /dev/null 2>&1 &')
  end
  return true
end

-- Makes sure the relay is running. With wait=true blocks up to ~2.5s until it answers.
local function ensure_relay(wait)
  if relay_alive() then
    send("stay")
    return true
  end
  if not launch() then return false end
  if wait then
    for _ = 1, 10 do
      if relay_alive() then return true end
    end
    return false
  end
  return true
end

local restart_tries = 0
local function restart_tick()
  restart_tries = restart_tries + 1
  if not relay_alive() then
    obs.timer_remove(restart_tick)
    read_ports()
    launch()
  elseif restart_tries > 15 then
    obs.timer_remove(restart_tick)
    obs.script_log(obs.LOG_WARNING, L("the relay did not close for the restart", "o relay não fechou para reiniciar", "el relay no se cerró para reiniciarse"))
  end
end

local function restart_relay()
  if obs.obs_frontend_streaming_active() then return false end
  if not relay_alive() then
    read_ports()
    return launch()
  end
  send("quit")
  restart_tries = 0
  obs.timer_remove(restart_tick)
  obs.timer_add(restart_tick, 300)
  return true
end

---------------------------------------------------------------------------
-- OBS stream settings
---------------------------------------------------------------------------
local function current_service()
  local svc = obs.obs_frontend_get_streaming_service()
  if svc == nil then return nil end
  local st = obs.obs_service_get_settings(svc)
  local info = {
    type = obs.obs_service_get_type(svc),
    server = obs.obs_data_get_string(st, "server"),
    key = obs.obs_data_get_string(st, "key"),
    service = obs.obs_data_get_string(st, "service"),
  }
  obs.obs_data_release(st)
  return info, svc
end

local function obs_configured()
  local svc = current_service()
  return svc ~= nil and (svc.server or ""):find("127.0.0.1:" .. ports.rtmp, 1, true) ~= nil
end

local function set_service(type_id, data)
  local svc = obs.obs_service_create(type_id, "default_service", data, nil)
  if svc == nil then return false end
  obs.obs_frontend_set_streaming_service(svc)
  obs.obs_frontend_save_streaming_service()
  obs.obs_service_release(svc)
  return true
end

-- Shows the outcome of an action in the panel (ok = green toast, error = red).
local function report(msg, is_error)
  send("result " .. (is_error and "error" or "ok") .. "\t" .. msg)
  if S then obs.obs_data_set_string(S, "status_info", msg) end
end

local function report_error(msg)
  report(msg, true)
end

-- Recovery data lives with the profile, not in the shared relay folder.
local function profile_path()
  local ok, path = pcall(obs.obs_frontend_get_current_profile_path)
  if ok and path and path ~= "" then return path end
  report_error(L("Could not locate the OBS profile. No settings changed.",
    "Não foi possível localizar o perfil do OBS. Nenhuma configuração alterada.",
    "No se pudo localizar el perfil de OBS. No se cambió la configuración."))
  return nil
end

local function recovery_error(path)
  report_error(L("Could not save recovery data: ", "Não foi possível salvar os dados de recuperação: ",
    "No se pudieron guardar los datos de recuperación: ") .. path)
end

-- Same format as installer.rs: record exactly what we change before applying it.
-- If the user edited a field since our last write, their new value becomes the baseline.
local function record_change(path, key, before, applied, numeric)
  if before == applied then return true end
  local journal_path = path .. ".dd-changes.json"
  local journal = exists(journal_path) and obs.obs_data_create_from_json_file(journal_path) or nil
  if exists(journal_path) and (journal == nil or obs.obs_data_get_int(journal, "version") ~= 1) then
    if journal then obs.obs_data_release(journal) end
    recovery_error(journal_path)
    return false
  end
  if journal == nil then
    journal = obs.obs_data_create()
    obs.obs_data_set_int(journal, "version", 1)
    obs.obs_data_set_bool(journal, "legacy_backup", exists(path .. ".dd-backup"))
  end
  local fields = obs.obs_data_get_obj(journal, "fields")
  if exists(journal_path) and fields == nil then
    obs.obs_data_release(journal)
    recovery_error(journal_path)
    return false
  end
  fields = fields or obs.obs_data_create()
  local previous = obs.obs_data_get_obj(fields, key)
  if previous and not obs.obs_data_has_user_value(previous, "applied") then
    obs.obs_data_release(previous)
    obs.obs_data_release(fields)
    obs.obs_data_release(journal)
    recovery_error(journal_path)
    return false
  end
  local get = numeric and obs.obs_data_get_int or obs.obs_data_get_string
  local set = numeric and obs.obs_data_set_int or obs.obs_data_set_string
  local change
  if previous and before == get(previous, "applied") then
    change = previous
  else
    if previous then obs.obs_data_release(previous) end
    change = obs.obs_data_create()
    if before ~= nil then set(change, "before", before) end
  end
  set(change, "applied", applied)
  obs.obs_data_set_obj(fields, key, change)
  obs.obs_data_set_obj(journal, "fields", fields)
  local ok = obs.obs_data_save_json_safe(journal, journal_path, "tmp", nil)
  obs.obs_data_release(change)
  obs.obs_data_release(fields)
  obs.obs_data_release(journal)
  if not ok then recovery_error(journal_path) end
  return ok
end

local function record_ini(cfg, path, section, key, value)
  local before = nil
  if obs.config_has_user_value(cfg, section, key) then
    before = obs.config_get_string(cfg, section, key)
  end
  return record_change(path .. "/basic.ini", section .. "." .. key, before, value, false)
end

---------------------------------------------------------------------------
-- Platform limits. With the Twitch/YouTube/Kick service selected, OBS applies
-- the platform's rules to the encoder ("Apply service settings"). Streaming
-- to the relay uses a custom server, so OBS stops doing it: a 10000 kbps,
-- 4 s keyframe 1080p60 stream is off-spec for Twitch and viewers drop to 720p.
-- This keeps those rules while OBS streams through the relay.
---------------------------------------------------------------------------
-- Kick ingests are "<12 hex>.global-contribute.live-video.net"; Twitch uses
-- live-video.net too ("ingest.global-contribute...", regional "*.contribute...").
local function is_kick_host(h)
  return h:find("^" .. ("%x"):rep(12) .. "%.global%-contribute%.live%-video%.net$") ~= nil or h:find("kick%.com$") ~= nil
end
local PLATFORMS = {
  { name = "Kick", max = 8000, match = is_kick_host },
  { name = "Twitch", max = 6000, match = function(h) return h:find("twitch%.tv$") or h:find("contribute%.live%-video%.net$") end },
  { name = "YouTube", max = nil, match = function(h) return h:find("youtube%.com$") end },
}

local function platform_limits(service_name)
  local names, max = {}, nil
  local function add(p)
    if not names[p.name] then names[p.name] = true; table.insert(names, p.name) end
    if p.max and (max == nil or p.max < max) then max = p.max end
  end
  for _, p in ipairs(PLATFORMS) do
    if service_name and service_name:lower():find(p.name:lower(), 1, true) == 1 then add(p) end
  end
  for _, u in ipairs(dest_urls) do
    local host = (u:match("^%a+://([^/:]+)") or ""):lower()
    for _, p in ipairs(PLATFORMS) do
      if p.match(host) then
        add(p)
        break
      end
    end
  end
  if #names == 0 then return nil end
  return { names = table.concat(names, " + "), max = max }
end

-- Returns a note describing what changed, or nil.
local function apply_platform_limits(service_name)
  local lim = platform_limits(service_name)
  if lim == nil then return nil end
  local cfg = obs.obs_frontend_get_profile_config()
  if cfg == nil then return nil end
  local advanced = obs.config_get_string(cfg, "Output", "Mode") == "Advanced"
  -- the streamer can still opt out, as with a normal service
  if advanced and not obs.config_get_bool(cfg, "AdvOut", "ApplyServiceSettings") then return nil end
  if not advanced and obs.config_get_bool(cfg, "Stream1", "IgnoreRecommended") then return nil end
  local changes = {}

  if advanced then
    local dir_ = profile_path()
    local enc_id = obs.config_get_string(cfg, "AdvOut", "Encoder")
    if dir_ and enc_id ~= "" then
      local path = dir_ .. "/streamEncoder.json"
      local file = obs.obs_data_create_from_json_file_safe(path, "bak")
      if file == nil and exists(path) then recovery_error(path); return nil, false end
      file = file or obs.obs_data_create()
      -- effective values: what is saved, over the encoder's defaults
      local eff = obs.obs_encoder_defaults(enc_id) or obs.obs_data_create()
      obs.obs_data_apply(eff, file)
      local bitrate = obs.obs_data_get_int(eff, "bitrate")
      local keyint = obs.obs_data_get_int(eff, "keyint_sec")
      local function remember(key, applied)
        local before = nil
        if obs.obs_data_has_user_value(file, key) then before = obs.obs_data_get_int(file, key) end
        return record_change(path, key, before, applied, true)
      end
      if (lim.max and bitrate > lim.max and not remember("bitrate", lim.max))
        or ((keyint == 0 or keyint > 2) and not remember("keyint_sec", 2)) then
        obs.obs_data_release(eff)
        obs.obs_data_release(file)
        return nil, false
      end
      if lim.max and bitrate > lim.max then
        obs.obs_data_set_int(file, "bitrate", lim.max)
        table.insert(changes, bitrate .. " -> " .. lim.max .. " kbps")
      end
      if keyint == 0 or keyint > 2 then
        obs.obs_data_set_int(file, "keyint_sec", 2)
        table.insert(changes, L("keyframe every 2 s", "keyframe a cada 2 s", "keyframe cada 2 s"))
      end
      if #changes > 0 then
        if not obs.obs_data_save_json_safe(file, path, "tmp", "bak") then
          recovery_error(path)
          obs.obs_data_release(eff)
          obs.obs_data_release(file)
          return nil, false
        end
        -- the encoder OBS already created, for this very stream
        local out = obs.obs_frontend_get_streaming_output()
        if out ~= nil then
          local enc = obs.obs_output_get_video_encoder(out)
          if enc ~= nil then obs.obs_encoder_update(enc, file) end
          obs.obs_output_release(out)
        end
      end
      obs.obs_data_release(eff)
      obs.obs_data_release(file)
    end
  elseif lim.max then
    -- simple mode always uses 2 s keyframes; only the bitrate needs a cap
    local bitrate = obs.config_get_int(cfg, "SimpleOutput", "VBitrate")
    if bitrate > lim.max then
      local path = profile_path()
      if not path or not record_ini(cfg, path, "SimpleOutput", "VBitrate", tostring(lim.max)) then return nil, false end
      obs.config_set_int(cfg, "SimpleOutput", "VBitrate", lim.max)
      if obs.config_save_safe(cfg, "tmp", nil) ~= 0 then
        recovery_error(path .. "/basic.ini")
        return nil, false
      end
      table.insert(changes, bitrate .. " -> " .. lim.max .. " kbps")
    end
  end
  if #changes == 0 then return nil end
  local note = L("encoder set to the ", "encoder ajustado às regras da ", "encoder ajustado a las reglas de ") .. lim.names .. L(" rules (", " (", " (") .. table.concat(changes, ", ") .. ")"
  obs.script_log(obs.LOG_INFO, note)
  return note
end

local function configure_obs()
  if obs.obs_frontend_streaming_active() then
    report_error(L("Stop the stream before configuring OBS.", "Pare a live antes de configurar o OBS.", "Detén la transmisión antes de configurar OBS."))
    return
  end
  local notes = {}
  local info, svc = current_service()
  local path = profile_path()
  if not path then return end
  local cfg = obs.obs_frontend_get_profile_config()
  if not cfg or not info then
    report_error(L("Could not read the OBS profile.", "Não foi possível ler o perfil do OBS.", "No se pudo leer el perfil de OBS."))
    return
  end
  local imported_service = nil
  if info and not obs_configured() then
    -- keep the original settings (same format as service.json) so they can be restored
    local backup = obs.obs_data_create()
    local st = obs.obs_service_get_settings(svc)
    obs.obs_data_set_string(backup, "type", info.type)
    obs.obs_data_set_obj(backup, "settings", st)
    local backup_path = path .. "/service.json.dd-backup"
    local ok = true
    if not exists(backup_path) then
      ok = obs.obs_data_save_json_safe(backup, backup_path, "tmp", nil)
    else
      -- Do not silently trust a broken backup or overwrite the first original.
      local kept = obs.obs_data_create_from_json_file(backup_path)
      ok = kept ~= nil and obs.obs_data_get_string(kept, "type") ~= ""
      if kept then obs.obs_data_release(kept) end
    end
    obs.obs_data_release(st)
    obs.obs_data_release(backup)
    if not ok then recovery_error(backup_path); return end
    table.insert(notes, L("destination and key imported from OBS", "destino e chave importados do OBS", "destino y clave importados de OBS"))
    imported_service = info.service
  end

  if not record_ini(cfg, path, "Output", "DelayEnable", "false") then return end
  if not obs_configured() then
    -- Import only after the recovery data is safely on disk.
    send_on(poll_sock, "import\t" .. (info.server or "") .. "\t" .. (info.key or "") .. "\t" .. (info.service or ""))
  end

  local data = obs.obs_data_create()
  obs.obs_data_set_string(data, "server", relay_server())
  obs.obs_data_set_string(data, "key", "delay")
  obs.obs_data_set_bool(data, "use_auth", false)
  local service_ok = set_service("rtmp_custom", data)
  obs.obs_data_release(data)
  if not service_ok then
    report_error(L("OBS could not create the streaming service.", "O OBS não conseguiu criar o serviço de transmissão.", "OBS no pudo crear el servicio de transmisión."))
    return
  end
  table.insert(notes, L("OBS streams to ", "OBS transmite para ", "OBS transmite a ") .. relay_server())

  obs.config_set_bool(cfg, "Output", "DelayEnable", false)
  if obs.config_save_safe(cfg, "tmp", nil) ~= 0 then recovery_error(path .. "/basic.ini"); return end
  table.insert(notes, L("built-in Stream Delay turned off", "Stream Delay nativo desligado", "Retraso de transmisión nativo desactivado"))
  local limits, limits_ok = apply_platform_limits(imported_service)
  if limits_ok == false then return end
  if limits then table.insert(notes, limits) end
  report(L("Done! ", "Pronto! ", "¡Listo! ") .. table.concat(notes, "; ") .. ".")
end

local function restore_obs()
  if obs.obs_frontend_streaming_active() then
    report_error(L("Stop the stream before restoring.", "Pare a live antes de restaurar.", "Detén la transmisión antes de restaurar."))
    return
  end
  local path = profile_path()
  if not path then return end
  local backup_path = path .. "/service.json.dd-backup"
  if not exists(backup_path) then backup_path = dir() .. "obs-service-backup.json" end
  if not exists(backup_path) then
    report_error(L("No original settings were saved.", "Não há configuração original salva.", "No hay configuración original guardada."))
    return
  end
  local backup = obs.obs_data_create_from_json_file(backup_path)
  if backup == nil then recovery_error(backup_path); return end
  local t = obs.obs_data_get_string(backup, "type")
  local st = obs.obs_data_get_obj(backup, "settings")
  if t ~= "" and st ~= nil then
    if set_service(t, st) then
      -- OBS' save API returns no status. Keep the backup until uninstall completes.
      report(L("Original stream settings restored.", "Configuração original de transmissão restaurada.", "Configuración de transmisión original restaurada."))
    else
      report_error(L("OBS could not restore the streaming service; backup kept.",
        "O OBS não conseguiu restaurar o serviço de transmissão; backup mantido.",
        "OBS no pudo restaurar el servicio de transmisión; copia conservada."))
    end
  else
    report_error(L("The backup of the original settings is empty.", "O backup da configuração original está vazio.", "La copia de seguridad de la configuración original está vacía."))
  end
  if st ~= nil then obs.obs_data_release(st) end
  obs.obs_data_release(backup)
end

---------------------------------------------------------------------------
-- Delay scene: shown while the delay builds up (grow mode "scene")
---------------------------------------------------------------------------
local shown_scene, previous_scene = nil, nil

local function program_scene_name()
  local cur = obs.obs_frontend_get_current_scene()
  if cur == nil then return nil end
  local name = obs.obs_source_get_name(cur)
  obs.obs_source_release(cur)
  return name
end

-- Puts a scene on program, also in studio mode.
local function set_program(src)
  if obs.obs_frontend_preview_program_mode_active() then
    obs.obs_frontend_set_current_preview_scene(src)
    obs.obs_frontend_preview_program_trigger_transition()
  else
    obs.obs_frontend_set_current_scene(src)
  end
end

local function show_scene(name)
  local src = obs.obs_get_source_by_name(name)
  if src == nil then
    send("scene_failed " .. name)
    report_error(L("The delay scene \"", "A cena de delay \"", "La escena de delay \"") .. name ..
      L("\" does not exist; the live picture was frozen instead.", "\" não existe; congelei a imagem da live.", "\" no existe; se congeló la imagen del directo."))
    return
  end
  previous_scene = program_scene_name()
  if previous_scene == name then previous_scene = nil end
  shown_scene = name
  set_program(src)
  obs.obs_source_release(src)
  send("scene_shown")
end

local function scene_back()
  -- only switch back if the streamer did not change scenes in the meantime
  if previous_scene and program_scene_name() == shown_scene then
    local src = obs.obs_get_source_by_name(previous_scene)
    if src ~= nil then
      set_program(src)
      obs.obs_source_release(src)
    end
  end
  shown_scene, previous_scene = nil, nil
end

-- Panic: cover scene + mute every audio source, undone by unpanic.
local panic_prev, panic_scene_name, panic_muted = nil, nil, {}

local function panic_on(mute, scene)
  if scene ~= "" then
    local src = obs.obs_get_source_by_name(scene)
    if src ~= nil then
      -- a second panic keeps the scene to go back to (not the panic scene itself)
      if panic_scene_name == nil then panic_prev = program_scene_name() end
      panic_scene_name = scene
      set_program(src)
      obs.obs_source_release(src)
    else
      report_error(L("Panic scene \"", "Cena de pânico \"", "Escena de pánico \"") .. scene .. L("\" not found.", "\" não encontrada.", "\" no encontrada."))
    end
  end
  if mute then
    -- a second panic adds to the list, so what the first one muted is unmuted later too
    local sources = obs.obs_enum_sources()
    if sources ~= nil then
      for _, src in ipairs(sources) do
        local flags = obs.obs_source_get_output_flags(src)
        if bit.band(flags, obs.OBS_SOURCE_AUDIO) ~= 0 and not obs.obs_source_muted(src) then
          obs.obs_source_set_muted(src, true)
          table.insert(panic_muted, obs.obs_source_get_name(src))
        end
      end
      obs.source_list_release(sources)
    end
  end
end

local function panic_off()
  for _, name in ipairs(panic_muted) do
    local src = obs.obs_get_source_by_name(name)
    if src ~= nil then
      obs.obs_source_set_muted(src, false)
      obs.obs_source_release(src)
    end
  end
  panic_muted = {}
  if panic_prev and program_scene_name() == panic_scene_name then
    local src = obs.obs_get_source_by_name(panic_prev)
    if src ~= nil then
      set_program(src)
      obs.obs_source_release(src)
    end
  end
  panic_prev, panic_scene_name = nil, nil
end

local last_program = nil
local function send_program()
  local name = program_scene_name()
  if name and name ~= last_program then
    last_program = name
    send("program\t" .. name)
  end
end

-- Phone deck actions -------------------------------------------------------
local function deck_scene(name)
  local src = obs.obs_get_source_by_name(name)
  if src == nil then
    report_error(L("Scene \"", "Cena \"", "Escena \"") .. name .. L("\" not found.", "\" não encontrada.", "\" no encontrada."))
    return
  end
  set_program(src)
  obs.obs_source_release(src)
end

local function deck_mute(name)
  local src = obs.obs_get_source_by_name(name)
  if src == nil then
    report_error(L("Audio source \"", "Fonte de áudio \"", "Fuente de audio \"") .. name .. L("\" not found.", "\" não encontrada.", "\" no encontrada."))
    return
  end
  obs.obs_source_set_muted(src, not obs.obs_source_muted(src))
  obs.obs_source_release(src)
end

local function deck_stream()
  if obs.obs_frontend_streaming_active() then obs.obs_frontend_streaming_stop() else obs.obs_frontend_streaming_start() end
end

local function deck_record()
  if obs.obs_frontend_recording_active() then obs.obs_frontend_recording_stop() else obs.obs_frontend_recording_start() end
end

-- Output size and frame rate: "<width>\t<height>\t<fps num>\t<fps den>" (zeros if unknown).
local function video_info()
  local ok, r = pcall(function()
    local ovi = obs.obs_video_info()
    if not obs.obs_get_video_info(ovi) then return nil end
    return ovi.output_width .. "\t" .. ovi.output_height .. "\t" .. ovi.fps_num .. "\t" .. ovi.fps_den
  end)
  return (ok and r) or "0\t0\t0\t0"
end

-- Clips copy the stream, so their frame rate is the OBS frame rate (Settings > Video).
-- On-screen widget: a Browser Source showing the relay's /overlay page.
local OVERLAY_NAME = "Dynamic Delay overlay"
local function add_overlay(url)
  local scene_src = obs.obs_frontend_get_current_scene()
  if scene_src == nil then return end
  local scene = obs.obs_scene_from_source(scene_src)
  local src = obs.obs_get_source_by_name(OVERLAY_NAME)
  if src == nil then
    local st = obs.obs_data_create()
    obs.obs_data_set_string(st, "url", url)
    obs.obs_data_set_int(st, "width", 640)
    obs.obs_data_set_int(st, "height", 160)
    src = obs.obs_source_create("browser_source", OVERLAY_NAME, st, nil)
    obs.obs_data_release(st)
  else
    local st = obs.obs_source_get_settings(src)
    obs.obs_data_set_string(st, "url", url)
    obs.obs_source_update(src, st)
    obs.obs_data_release(st)
  end
  if src ~= nil then
    local item = obs.obs_scene_find_source(scene, OVERLAY_NAME)
    if item == nil then
      item = obs.obs_scene_add(scene, src)
      local pos = obs.vec2()
      pos.x, pos.y = 24, 24
      obs.obs_sceneitem_set_pos(item, pos)
    end
    obs.obs_sceneitem_set_visible(item, true)
    obs.obs_source_release(src)
  end
  obs.obs_source_release(scene_src)
end

local function set_fps(n)
  local busy = obs.obs_frontend_streaming_active() or obs.obs_frontend_recording_active()
  pcall(function() busy = busy or obs.obs_frontend_replay_buffer_active() or obs.obs_frontend_virtualcam_active() end)
  if busy then
    report_error(L("Stop the stream and the recording before changing the frame rate.", "Pare a live e a gravação antes de mudar o FPS.", "Detén la transmisión y la grabación antes de cambiar los FPS."))
    return
  end
  local cfg = obs.obs_frontend_get_profile_config()
  obs.config_set_uint(cfg, "Video", "FPSType", 0) -- "Common FPS values"
  obs.config_set_string(cfg, "Video", "FPSCommon", tostring(n))
  pcall(obs.config_save_safe, cfg, "tmp", nil)
  obs.obs_frontend_reset_video()
  report(L("OBS now runs at ", "O OBS agora roda a ", "OBS ahora funciona a ") .. n .. L(" FPS (Settings > Video).", " FPS (Configurações > Vídeo).", " FPS (Configuración > Video)."))
end

-- Audio sources with their mute state, and whether OBS streams/records, for the deck keys.
local last_audio, last_obsstate = nil, nil
local function send_obs_state(force)
  local parts = {}
  local sources = obs.obs_enum_sources()
  if sources ~= nil then
    for _, src in ipairs(sources) do
      if bit.band(obs.obs_source_get_output_flags(src), obs.OBS_SOURCE_AUDIO) ~= 0 then
        table.insert(parts, obs.obs_source_get_name(src) .. "=" .. (obs.obs_source_muted(src) and "1" or "0"))
      end
    end
    obs.source_list_release(sources)
  end
  local audio = "audio\t" .. table.concat(parts, "\t")
  if force or audio ~= last_audio then
    last_audio = audio
    send(audio)
  end
  local state = "obsstate\t" .. (obs.obs_frontend_streaming_active() and "1" or "0") .. "\t"
    .. (obs.obs_frontend_recording_active() and "1" or "0") .. "\t" .. video_info()
  if force or state ~= last_obsstate then
    last_obsstate = state
    send(state)
  end
end

local function send_scene_list()
  local scenes = obs.obs_frontend_get_scenes()
  if scenes == nil then return end
  local names = {}
  for _, src in ipairs(scenes) do
    table.insert(names, obs.obs_source_get_name(src))
  end
  obs.source_list_release(scenes)
  send("scenes\t" .. table.concat(names, "\t"))
end

---------------------------------------------------------------------------
-- Link with the panel: poll the relay for actions to run inside OBS
---------------------------------------------------------------------------
local polls = 0
local function poll_tick()
  local reply = recv_on(poll_sock, 1)
  while reply do
    if reply == "configure" then configure_obs()
    elseif reply == "restore" then restore_obs()
    elseif reply:sub(1, 11) == "scene_show\t" then show_scene(reply:sub(12))
    elseif reply == "scene_back" then scene_back()
    elseif reply:sub(1, 6) == "panic\t" then
      local mute, scene = reply:match("^panic\t(%d)\t(.*)$")
      panic_on(mute == "1", scene or "")
    elseif reply == "unpanic" then panic_off()
    elseif reply:sub(1, 6) == "scene\t" then deck_scene(reply:sub(7))
    elseif reply:sub(1, 5) == "mute\t" then deck_mute(reply:sub(6)); send_obs_state(true)
    elseif reply == "stream_toggle" then deck_stream()
    elseif reply == "record_toggle" then deck_record()
    elseif reply:sub(1, 4) == "fps\t" then set_fps(tonumber(reply:sub(5)) or 60); send_obs_state(true)
    elseif reply:sub(1, 8) == "overlay\t" then add_overlay(reply:sub(9)) end
    reply = recv_on(poll_sock, 1)
  end
  send_on(poll_sock, "poll " .. (obs_configured() and "1" or "0"))
  send_program()
  send_obs_state(polls % 10 == 0)
  if polls % 10 == 0 then
    last_program = nil -- resend now and then, in case the relay restarted
    send_scene_list()
    read_ports() -- picks up a language change made in the panel
  end
  polls = polls + 1
end

---------------------------------------------------------------------------
-- Hotkeys
---------------------------------------------------------------------------
-- Descriptions are resolved at load time, in the configured language.
local actions = {
  { id = "dyn_delay_toggle", en = "Dynamic Delay: toggle on/off", pt = "Delay dinâmico: ligar/desligar",
    es = "Delay dinámico: activar/desactivar",
    cmd = function() return "toggle" end },
  { id = "dyn_delay_on", en = "Dynamic Delay: turn on", pt = "Delay dinâmico: ligar",
    es = "Delay dinámico: activar", cmd = function() return "on" end },
  { id = "dyn_delay_off", en = "Dynamic Delay: turn off (back to live)", pt = "Delay dinâmico: desligar (voltar ao vivo)",
    es = "Delay dinámico: desactivar (volver al directo)",
    cmd = function() return "off" end },
  { id = "dyn_delay_plus", en = "Dynamic Delay: increase", pt = "Delay dinâmico: aumentar", es = "Delay dinámico: aumentar",
    cmd = function() return "add " .. step end },
  { id = "dyn_delay_minus", en = "Dynamic Delay: decrease", pt = "Delay dinâmico: diminuir", es = "Delay dinámico: disminuir",
    cmd = function() return "add -" .. step end },
  { id = "dyn_delay_censor", en = "Dynamic Delay: delete before it airs", pt = "Delay dinâmico: apagar antes de ir ao ar",
    es = "Delay dinámico: borrar antes de salir al aire",
    cmd = function() return "censor" end },
  { id = "dyn_delay_replay", en = "Dynamic Delay: instant replay", pt = "Delay dinâmico: replay instantâneo",
    es = "Delay dinámico: repetición instantánea",
    cmd = function() return "replay" end },
  { id = "dyn_delay_clip", en = "Dynamic Delay: save clip", pt = "Delay dinâmico: salvar clipe", es = "Delay dinámico: guardar clip",
    cmd = function() return "clip" end },
  { id = "dyn_delay_panic", en = "Dynamic Delay: panic button", pt = "Delay dinâmico: botão de pânico",
    es = "Delay dinámico: botón de pánico",
    cmd = function() return "panic" end },
}

---------------------------------------------------------------------------
-- OBS script API
---------------------------------------------------------------------------
function script_description()
  read_ports()
  return L([[<h2>Dynamic Delay</h2>
<p>Settings live in the <b>Dynamic Delay</b> panel (<i>Docks</i> menu).
Hotkeys in <i>Settings &gt; Hotkeys &gt; Dynamic Delay</i>.</p>
<p>Developed by <a href="https://github.com/ragnarcb">ragnarcb</a></p>]], [[<h2>Delay dinâmico</h2>
<p>A configuração fica no painel <b>Delay dinâmico</b> (menu <i>Docks</i>).
Atalhos em <i>Configurações &gt; Atalhos &gt; Delay dinâmico</i>.</p>
<p>Desenvolvido por <a href="https://github.com/ragnarcb">ragnarcb</a></p>]], [[<h2>Delay dinámico</h2>
<p>La configuración está en el panel <b>Delay dinámico</b> (menú <i>Docks</i>).
Atajos en <i>Configuración &gt; Atajos &gt; Delay dinámico</i>.</p>
<p>Desarrollado por <a href="https://github.com/ragnarcb">ragnarcb</a></p>]])
end

local function refresh_status()
  local r = request("status", 400)
  if r == nil then
    r = exists(exe_path()) and L("Relay closed.", "Relay fechado.", "Relay cerrado.")
      or (L("Relay not found: ", "Relay não encontrado: ", "Relay no encontrado: ") .. exe_path())
  end
  if not obs_configured() then
    r = r .. L("\nOBS NOT configured yet: click \"Configure OBS automatically\".",
      "\nOBS AINDA NÃO configurado: clique em \"Configurar o OBS automaticamente\".",
      "\nOBS TODAVÍA NO está configurado: haz clic en \"Configurar OBS automáticamente\".")
  end
  obs.obs_data_set_string(S, "status_info", r)
end

function script_properties()
  if S then refresh_status() end
  local p = obs.obs_properties_create()
  obs.obs_properties_add_text(p, "status_info", L("Status", "Status", "Estado"), obs.OBS_TEXT_INFO)
  obs.obs_properties_add_button(p, "btn_refresh", L("Refresh status", "Atualizar status", "Actualizar estado"), function()
    refresh_status(); return true
  end)
  obs.obs_properties_add_button(p, "btn_toggle", L("Toggle delay now", "Ligar/desligar delay agora", "Activar/desactivar delay ahora"), function()
    send("toggle"); refresh_status(); return true
  end)
  obs.obs_properties_add_button(p, "btn_configure", L("Configure OBS automatically", "Configurar o OBS automaticamente", "Configurar OBS automáticamente"), function()
    configure_obs(); return true
  end)
  obs.obs_properties_add_button(p, "btn_restore", L("Restore original OBS settings", "Restaurar configuração original do OBS", "Restaurar configuración original de OBS"), function()
    restore_obs(); return true
  end)
  obs.obs_properties_add_button(p, "btn_panel", L("Open panel in the browser", "Abrir painel no navegador", "Abrir panel en el navegador"), function()
    read_ports()
    open_target("http://127.0.0.1:" .. ports.http .. "/?token=" .. api_token); return false
  end)
  obs.obs_properties_add_int(p, "step", L("Increase/decrease step (s)", "Passo do aumentar/diminuir (s)", "Paso para aumentar/disminuir (s)"), 1, 120, 1)
  obs.obs_properties_add_bool(p, "manage_relay", L("Start and close the relay with OBS", "Abrir e fechar o relay junto com o OBS", "Abrir y cerrar el relay junto con OBS"))
  obs.obs_properties_add_button(p, "btn_restart", L("Restart relay", "Reiniciar relay", "Reiniciar relay"), function()
    if not restart_relay() then obs.obs_data_set_string(S, "status_info", L("Cannot restart during a stream.", "Não dá para reiniciar durante a live.", "No se puede reiniciar durante la transmisión.")) end
    return true
  end)
  obs.obs_properties_add_path(p, "relay_path",
    L("Relay executable (empty = same folder as the script)", "Executável do relay (vazio = mesma pasta do script)", "Ejecutable del relay (vacío = misma carpeta que el script)"),
    obs.OBS_PATH_FILE, L("Executable (*.exe);;All (*.*)", "Executável (*.exe);;Todos (*.*)", "Ejecutable (*.exe);;Todos (*.*)"), nil)
  return p
end

function script_defaults(s)
  obs.obs_data_set_default_int(s, "step", 5)
  obs.obs_data_set_default_bool(s, "manage_relay", true)
end

function script_update(s)
  S = s
  step = obs.obs_data_get_int(s, "step")
  manage_relay = obs.obs_data_get_bool(s, "manage_relay")
  relay_path = obs.obs_data_get_string(s, "relay_path")
end

local function on_event(event)
  if event == obs.OBS_FRONTEND_EVENT_STREAMING_STARTING and obs_configured() then
    read_ports() -- the destination may have changed in the panel
    apply_platform_limits()
  end
  if event == obs.OBS_FRONTEND_EVENT_STREAMING_STARTING and manage_relay and obs_configured() then
    if not ensure_relay(true) then
      obs.script_log(obs.LOG_WARNING, L("relay did not start; the stream will fail to connect", "relay não iniciou; a live vai falhar ao conectar", "el relay no se inició; la transmisión no podrá conectarse"))
    end
  end
end

function script_load(s)
  script_update(s)
  read_ports()
  for _, a in ipairs(actions) do
    a.hk = obs.obs_hotkey_register_frontend(a.id, L(a.en, a.pt, a.es), function(pressed)
      if pressed then send(a.cmd()) end
    end)
    local arr = obs.obs_data_get_array(s, a.id)
    obs.obs_hotkey_load(a.hk, arr)
    obs.obs_data_array_release(arr)
  end
  obs.obs_frontend_add_event_callback(on_event)
  if obs_configured() then apply_platform_limits() end
  if manage_relay then
    if relay_alive() and not obs.obs_frontend_streaming_active() then
      restart_relay() -- make sure the running relay is this version with this config
    else
      ensure_relay(false)
    end
  end
  obs.timer_add(poll_tick, 300)
end

function script_save(s)
  for _, a in ipairs(actions) do
    local arr = obs.obs_hotkey_save(a.hk)
    obs.obs_data_set_array(s, a.id, arr)
    obs.obs_data_array_release(arr)
  end
end

function script_unload()
  obs.timer_remove(poll_tick)
  obs.timer_remove(restart_tick)
  if manage_relay then
    send("quit") -- the relay exits after any delayed tail has been sent
  end
  for _, s in ipairs({ cmd_sock, poll_sock }) do
    if is_windows then sock_lib.closesocket(s) else sock_lib.close(s) end
  end
end
