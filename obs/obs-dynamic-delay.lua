-- obs-dynamic-delay: configura e controla o delay dinâmico de dentro do OBS.
-- Ferramentas > Scripts > "+" > escolha este arquivo (deixe o executável na mesma pasta).

obs = obslua
local ffi = require("ffi")

local is_windows = ffi.os == "Windows"
local EXE_NAME = is_windows and "obs-dynamic-delay.exe" or "obs-dynamic-delay"

local PLATFORMS = {
  { id = "twitch", name = "Twitch", url = "rtmp://live.twitch.tv/app" },
  { id = "youtube", name = "YouTube", url = "rtmp://a.rtmp.youtube.com/live2" },
  { id = "kick", name = "Kick (cole a URL do painel da Kick)", url = nil },
  { id = "custom", name = "Outra (URL personalizada)", url = nil },
}

-- current settings (filled by script_update)
local S = nil
local c = {}
local applied_sig = nil
local last_delay_sent = nil

---------------------------------------------------------------------------
-- Sockets (LuaJIT FFI, no extra dependencies)
---------------------------------------------------------------------------
local sock_lib, shell32, sock = nil, nil, nil

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

local function get_sock()
  if sock == nil then
    sock = sock_lib.socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP)
    if is_windows then
      -- SIO_UDP_CONNRESET off: an ICMP "port unreachable" must not break later reads
      local off = ffi.new("uint32_t[1]", 0)
      sock_lib.WSAIoctl(sock, 0x9800000C, off, 4, nil, 0, ffi.new("uint32_t[1]"), nil, nil)
    end
  end
  return sock
end

local function set_timeout(ms)
  if is_windows then
    local v = ffi.new("uint32_t[1]", ms)
    sock_lib.setsockopt(get_sock(), 0xffff, 0x1006, v, 4)
  else
    local tv = ffi.new("dd_timeval")
    tv.sec = math.floor(ms / 1000)
    tv.usec = (ms % 1000) * 1000
    local level, opt = 1, 20 -- Linux SOL_SOCKET, SO_RCVTIMEO
    if ffi.os == "OSX" then level, opt = 0xffff, 0x1006 end
    sock_lib.setsockopt(get_sock(), level, opt, tv, ffi.sizeof(tv))
  end
end

local function send(cmd)
  local addr = ffi.new("dd_sockaddr_in")
  if ffi.os == "OSX" then addr.len = ffi.sizeof(addr) end
  addr.family = AF_INET
  addr.port = sock_lib.htons(c.udp_port or 8788)
  addr.addr = sock_lib.inet_addr("127.0.0.1")
  return tonumber(sock_lib.sendto(get_sock(), cmd, #cmd, 0, addr, ffi.sizeof(addr))) >= 0
end

-- Sends a command and waits for a reply. Returns nil on timeout.
local function request(cmd, timeout_ms)
  local s = get_sock()
  local buf = ffi.new("char[2048]")
  -- drop stale replies
  set_timeout(1)
  while tonumber(sock_lib.recvfrom(s, buf, 2048, 0, nil, nil)) > 0 do end
  if not send(cmd) then return nil end
  set_timeout(timeout_ms)
  local n = tonumber(sock_lib.recvfrom(s, buf, 2048, 0, nil, nil))
  if n == nil or n <= 0 then return nil end
  return ffi.string(buf, n)
end

local function relay_alive()
  return request("status", 250) ~= nil
end

---------------------------------------------------------------------------
-- Files and processes
---------------------------------------------------------------------------
local function script_dir()
  return script_path()
end

local function exists(path)
  local f = io.open(path, "rb")
  if f then f:close() return true end
  return false
end

local function exe_path()
  if c.relay_path and c.relay_path ~= "" then return c.relay_path end
  return script_dir() .. EXE_NAME
end

local function config_path()
  return script_dir() .. "config.toml"
end

local function upstream_url()
  for _, p in ipairs(PLATFORMS) do
    if p.id == c.platform and p.url then return p.url end
  end
  return c.custom_url or ""
end

local function relay_server()
  return "rtmp://127.0.0.1:" .. c.rtmp_port .. "/live"
end

local function toml_str(s)
  s = s:gsub("\\", "\\\\")
  s = s:gsub('"', '\\"')
  return '"' .. s .. '"'
end

-- Settings that need a relay restart to take effect.
local function config_sig()
  return table.concat({ upstream_url(), c.stream_key, tostring(c.start_enabled), c.rtmp_port, c.http_port, c.udp_port }, "|")
end

local function write_config()
  local f, err = io.open(config_path(), "w")
  if not f then
    obs.script_log(obs.LOG_WARNING, "nao consegui gravar " .. config_path() .. ": " .. tostring(err))
    return false
  end
  f:write("# Gerado pelo script do OBS. Edite pelas propriedades do script.\n")
  f:write("listen = " .. toml_str("127.0.0.1:" .. c.rtmp_port) .. "\n")
  f:write("upstream_url = " .. toml_str(upstream_url()) .. "\n")
  f:write("stream_key = " .. toml_str(c.stream_key) .. "\n")
  f:write("delay_seconds = " .. c.delay_seconds .. "\n")
  f:write("start_enabled = " .. tostring(c.start_enabled) .. "\n")
  f:write("max_delay_seconds = 600\n")
  f:write("filler_fps = 2\n")
  f:write("http_listen = " .. toml_str("127.0.0.1:" .. c.http_port) .. "\n")
  f:write("udp_listen = " .. toml_str("127.0.0.1:" .. c.udp_port) .. "\n")
  f:close()
  return true
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
    obs.script_log(obs.LOG_WARNING, "executavel do relay nao encontrado: " .. exe)
    return false
  end
  if not write_config() then return false end
  if is_windows then
    local dir = exe:match("^(.*)[\\/]")
    -- 0 = SW_HIDE: the relay runs in the background, logs go to obs-dynamic-delay.log
    local r = shell32.ShellExecuteA(nil, "open", exe, '"' .. config_path() .. '"', dir, 0)
    if tonumber(ffi.cast("intptr_t", r)) <= 32 then
      obs.script_log(obs.LOG_WARNING, "falha ao iniciar " .. exe)
      return false
    end
  else
    os.execute('"' .. exe .. '" "' .. config_path() .. '" > /dev/null 2>&1 &')
  end
  applied_sig = config_sig()
  last_delay_sent = c.delay_seconds
  return true
end

-- Makes sure the relay is running. With wait=true blocks up to ~2s until it answers.
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

local function streaming()
  return obs.obs_frontend_streaming_active()
end

-- Restart so a new destination/port takes effect (only while not streaming).
local restart_tries = 0
local function restart_tick()
  restart_tries = restart_tries + 1
  if not relay_alive() then
    obs.timer_remove(restart_tick)
    launch()
  elseif restart_tries > 15 then
    obs.timer_remove(restart_tick)
    obs.script_log(obs.LOG_WARNING, "o relay nao fechou para reiniciar")
  end
end

local function restart_relay()
  if streaming() then return false end
  write_config()
  if not relay_alive() then return launch() end
  send("quit")
  restart_tries = 0
  obs.timer_remove(restart_tick)
  obs.timer_add(restart_tick, 300)
  return true
end

---------------------------------------------------------------------------
-- OBS stream settings
---------------------------------------------------------------------------
local function points_to_relay(server)
  return server ~= nil and server:find("127.0.0.1:" .. c.rtmp_port, 1, true) ~= nil
end

local function current_service()
  local svc = obs.obs_frontend_get_streaming_service()
  if svc == nil then return nil end
  local st = obs.obs_service_get_settings(svc)
  local info = {
    type = obs.obs_service_get_type(svc),
    server = obs.obs_data_get_string(st, "server"),
    key = obs.obs_data_get_string(st, "key"),
    service = obs.obs_data_get_string(st, "service"),
    json = obs.obs_data_get_json(st),
  }
  obs.obs_data_release(st)
  return info
end

local function set_service(type_id, data)
  local svc = obs.obs_service_create(type_id, "default_service", data, nil)
  obs.obs_frontend_set_streaming_service(svc)
  obs.obs_frontend_save_streaming_service()
  obs.obs_service_release(svc)
end

-- Turns off OBS' own delay, which cannot change during a stream and would add up.
local function disable_builtin_delay()
  local ok = pcall(function()
    local cfg = obs.obs_frontend_get_profile_config()
    obs.config_set_bool(cfg, "Output", "DelayEnable", false)
  end)
  return ok
end

local function status_message(msg)
  obs.obs_data_set_string(S, "status_info", msg)
end

local function refresh_status()
  local r = request("status", 400)
  if r == nil then
    if not exists(exe_path()) then
      r = "Relay nao encontrado. Coloque " .. EXE_NAME .. " na mesma pasta deste script."
    else
      r = "Relay parado."
    end
  end
  local svc = current_service()
  if svc and points_to_relay(svc.server) then
    r = r .. "\nOBS configurado para o relay."
  else
    r = r .. "\nOBS AINDA NAO configurado: clique em \"Configurar o OBS automaticamente\"."
  end
  status_message(r)
end

local function configure_obs()
  if streaming() then
    status_message("Pare a live antes de configurar o OBS.")
    return
  end
  local svc = current_service()
  local notes = {}
  if svc and not points_to_relay(svc.server) then
    -- remember the original settings so they can be restored
    obs.obs_data_set_string(S, "backup_type", svc.type)
    obs.obs_data_set_string(S, "backup_json", svc.json)

    local server, key = svc.server or "", svc.key or ""
    if key ~= "" and c.stream_key == "" then
      obs.obs_data_set_string(S, "stream_key", key)
      table.insert(notes, "chave importada do OBS")
    end
    if server:match("^rtmps?://") then
      local matched = false
      for _, p in ipairs(PLATFORMS) do
        if p.url == server then
          obs.obs_data_set_string(S, "platform", p.id)
          matched = true
        end
      end
      if not matched then
        obs.obs_data_set_string(S, "platform", "custom")
        obs.obs_data_set_string(S, "custom_url", server)
      end
      table.insert(notes, "destino importado: " .. server)
    elseif (svc.service or ""):lower():find("twitch") then
      obs.obs_data_set_string(S, "platform", "twitch")
      table.insert(notes, "destino importado: Twitch")
    elseif (svc.service or ""):lower():find("youtube") then
      obs.obs_data_set_string(S, "platform", "youtube")
      table.insert(notes, "destino importado: YouTube")
    else
      table.insert(notes, "nao reconheci o destino atual, confira a plataforma abaixo")
    end
  end
  script_update(S)

  local data = obs.obs_data_create()
  obs.obs_data_set_string(data, "server", relay_server())
  obs.obs_data_set_string(data, "key", "delay")
  obs.obs_data_set_bool(data, "use_auth", false)
  set_service("rtmp_custom", data)
  obs.obs_data_release(data)
  table.insert(notes, "OBS agora transmite para " .. relay_server())

  if disable_builtin_delay() then
    table.insert(notes, "Stream Delay nativo do OBS desligado")
  end
  if c.stream_key == "" then
    table.insert(notes, "ATENCAO: preencha a chave de transmissao")
  end
  restart_relay()
  status_message("Pronto! " .. table.concat(notes, "; ") .. ".")
end

local function restore_obs()
  if streaming() then
    status_message("Pare a live antes de restaurar.")
    return
  end
  local t = obs.obs_data_get_string(S, "backup_type")
  local json = obs.obs_data_get_string(S, "backup_json")
  if t == "" or json == "" then
    status_message("Nao ha configuracao original salva.")
    return
  end
  local data = obs.obs_data_create_from_json(json)
  set_service(t, data)
  obs.obs_data_release(data)
  status_message("Configuracao original de transmissao restaurada. O delay dinamico nao sera usado.")
end

---------------------------------------------------------------------------
-- Hotkeys
---------------------------------------------------------------------------
local actions = {
  { id = "dyn_delay_toggle", desc = "Delay dinâmico: ligar/desligar", cmd = function() return "toggle" end },
  { id = "dyn_delay_on", desc = "Delay dinâmico: ligar", cmd = function() return "on" end },
  { id = "dyn_delay_off", desc = "Delay dinâmico: desligar (voltar ao vivo)", cmd = function() return "off" end },
  { id = "dyn_delay_plus", desc = "Delay dinâmico: aumentar", cmd = function() return "add " .. c.step end },
  { id = "dyn_delay_minus", desc = "Delay dinâmico: diminuir", cmd = function() return "add -" .. c.step end },
}

---------------------------------------------------------------------------
-- OBS script API
---------------------------------------------------------------------------
function script_description()
  return [[<h2>Delay dinâmico</h2>
<p>Ligue e desligue o delay da live a qualquer momento, sem parar a transmissão.</p>
<p><b>Primeira vez:</b> preencha plataforma e chave (ou deixe importar do OBS) e clique em
<b>Configurar o OBS automaticamente</b>. Depois defina os atalhos em
<i>Configurações &gt; Atalhos &gt; Delay dinâmico</i>.</p>]]
end

local function on_platform_changed(props, _, settings)
  local id = obs.obs_data_get_string(settings, "platform")
  obs.obs_property_set_visible(obs.obs_properties_get(props, "custom_url"), id == "kick" or id == "custom")
  return true
end

function script_properties()
  if S then refresh_status() end
  local p = obs.obs_properties_create()

  obs.obs_properties_add_text(p, "status_info", "Status", obs.OBS_TEXT_INFO)
  obs.obs_properties_add_button(p, "btn_refresh", "Atualizar status", function()
    refresh_status(); return true
  end)
  obs.obs_properties_add_button(p, "btn_toggle", "Ligar/desligar delay agora", function()
    send("toggle"); refresh_status(); return true
  end)

  local dest = obs.obs_properties_create()
  local list = obs.obs_properties_add_list(dest, "platform", "Plataforma", obs.OBS_COMBO_TYPE_LIST, obs.OBS_COMBO_FORMAT_STRING)
  for _, pl in ipairs(PLATFORMS) do obs.obs_property_list_add_string(list, pl.name, pl.id) end
  obs.obs_property_set_modified_callback(list, on_platform_changed)
  obs.obs_properties_add_text(dest, "custom_url", "URL do servidor (rtmp:// ou rtmps://)", obs.OBS_TEXT_DEFAULT)
  obs.obs_properties_add_text(dest, "stream_key", "Chave de transmissão", obs.OBS_TEXT_PASSWORD)
  obs.obs_properties_add_button(dest, "btn_configure", "Configurar o OBS automaticamente", function()
    configure_obs(); return true
  end)
  obs.obs_properties_add_button(dest, "btn_restore", "Restaurar configuração original do OBS", function()
    restore_obs(); return true
  end)
  obs.obs_properties_add_group(p, "grp_dest", "Destino da live", obs.OBS_GROUP_NORMAL, dest)

  local d = obs.obs_properties_create()
  obs.obs_properties_add_int_slider(d, "delay_seconds", "Delay (segundos)", 1, 600, 1)
  obs.obs_properties_add_bool(d, "start_enabled", "Começar toda live já com delay")
  obs.obs_properties_add_int(d, "step", "Passo do aumentar/diminuir (s)", 1, 120, 1)
  obs.obs_properties_add_button(d, "btn_panel", "Abrir painel no navegador", function()
    open_target("http://127.0.0.1:" .. c.http_port .. "/"); return false
  end)
  obs.obs_properties_add_group(p, "grp_delay", "Delay", obs.OBS_GROUP_NORMAL, d)

  local a = obs.obs_properties_create()
  obs.obs_properties_add_bool(a, "manage_relay", "Iniciar e fechar o relay junto com o OBS")
  obs.obs_properties_add_button(a, "btn_restart", "Reiniciar relay", function()
    if not restart_relay() then status_message("Nao da para reiniciar durante a live.") end
    return true
  end)
  obs.obs_properties_add_path(a, "relay_path", "Executável do relay (vazio = mesma pasta do script)",
    obs.OBS_PATH_FILE, "Executável (*.exe);;Todos (*.*)", nil)
  obs.obs_properties_add_int(a, "rtmp_port", "Porta RTMP local", 1, 65535, 1)
  obs.obs_properties_add_int(a, "http_port", "Porta do painel", 1, 65535, 1)
  obs.obs_properties_add_int(a, "udp_port", "Porta de comandos", 1, 65535, 1)
  obs.obs_properties_add_group(p, "grp_adv", "Avançado", obs.OBS_GROUP_NORMAL, a)

  on_platform_changed(p, nil, S)
  return p
end

function script_defaults(s)
  obs.obs_data_set_default_string(s, "platform", "twitch")
  obs.obs_data_set_default_int(s, "delay_seconds", 30)
  obs.obs_data_set_default_bool(s, "start_enabled", false)
  obs.obs_data_set_default_int(s, "step", 5)
  obs.obs_data_set_default_bool(s, "manage_relay", true)
  obs.obs_data_set_default_int(s, "rtmp_port", 1935)
  obs.obs_data_set_default_int(s, "http_port", 8787)
  obs.obs_data_set_default_int(s, "udp_port", 8788)
end

-- Applies changed settings a moment after the user stops typing.
local function apply_tick()
  obs.timer_remove(apply_tick)
  if c.delay_seconds ~= last_delay_sent and relay_alive() then
    send("set " .. c.delay_seconds)
    last_delay_sent = c.delay_seconds
  end
  write_config()
  if applied_sig ~= nil and config_sig() ~= applied_sig then
    if streaming() then
      -- takes effect on the next start
    elseif c.manage_relay or relay_alive() then
      restart_relay()
    end
  end
end

function script_update(s)
  S = s
  c.platform = obs.obs_data_get_string(s, "platform")
  c.custom_url = obs.obs_data_get_string(s, "custom_url")
  c.stream_key = obs.obs_data_get_string(s, "stream_key")
  c.delay_seconds = obs.obs_data_get_int(s, "delay_seconds")
  c.start_enabled = obs.obs_data_get_bool(s, "start_enabled")
  c.step = obs.obs_data_get_int(s, "step")
  c.manage_relay = obs.obs_data_get_bool(s, "manage_relay")
  c.relay_path = obs.obs_data_get_string(s, "relay_path")
  c.rtmp_port = obs.obs_data_get_int(s, "rtmp_port")
  c.http_port = obs.obs_data_get_int(s, "http_port")
  c.udp_port = obs.obs_data_get_int(s, "udp_port")
  obs.timer_remove(apply_tick)
  obs.timer_add(apply_tick, 1500)
end

local function on_event(event)
  if event == obs.OBS_FRONTEND_EVENT_STREAMING_STARTING and c.manage_relay then
    local svc = current_service()
    if svc and points_to_relay(svc.server) and not ensure_relay(true) then
      obs.script_log(obs.LOG_WARNING, "relay nao iniciou; a live vai falhar ao conectar")
    end
  end
end

function script_load(s)
  script_update(s)
  for _, a in ipairs(actions) do
    a.hk = obs.obs_hotkey_register_frontend(a.id, a.desc, function(pressed)
      if pressed then send(a.cmd()) end
    end)
    local arr = obs.obs_data_get_array(s, a.id)
    obs.obs_hotkey_load(a.hk, arr)
    obs.obs_data_array_release(arr)
  end
  obs.obs_frontend_add_event_callback(on_event)
  if c.manage_relay then
    if relay_alive() and not streaming() then
      restart_relay() -- make sure the running relay uses these settings
    else
      ensure_relay(false)
    end
  end
  applied_sig = config_sig()
end

function script_save(s)
  for _, a in ipairs(actions) do
    local arr = obs.obs_hotkey_save(a.hk)
    obs.obs_data_set_array(s, a.id, arr)
    obs.obs_data_array_release(arr)
  end
end

function script_unload()
  obs.timer_remove(apply_tick)
  obs.timer_remove(restart_tick)
  if c.manage_relay then
    -- the relay exits after any delayed tail has been sent
    send("quit")
  end
  if sock ~= nil then
    if is_windows then sock_lib.closesocket(sock) else sock_lib.close(sock) end
    sock = nil
  end
end
