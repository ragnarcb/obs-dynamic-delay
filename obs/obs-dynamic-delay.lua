-- obs-dynamic-delay: OBS hotkeys for the dynamic delay relay.
-- Tools > Scripts > "+" > choose this file. Then Settings > Hotkeys > "Delay dinâmico".

obs = obslua
local ffi = require("ffi")

local host = "127.0.0.1"
local port = 8788
local panel_url = "http://127.0.0.1:8787/"
local step = 5
local relay_path = ""
local autostart = false

---------------------------------------------------------------------------
-- UDP sender (LuaJIT FFI, no extra dependencies)
---------------------------------------------------------------------------
local is_windows = ffi.os == "Windows"
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
    int closesocket(SOCKET s);
    void *ShellExecuteA(void *hwnd, const char *op, const char *file, const char *params, const char *dir, int show);
  ]]
  sock_lib = ffi.load("ws2_32")
  shell32 = ffi.load("shell32")
  sock_lib.WSAStartup(0x0202, ffi.new("uint8_t[512]"))
elseif ffi.os == "OSX" then
  ffi.cdef [[
    typedef struct { uint8_t len; uint8_t family; uint16_t port; uint32_t addr; char zero[8]; } dd_sockaddr_in;
    int socket(int af, int type, int protocol);
    intptr_t sendto(int s, const void *buf, size_t len, int flags, const void *to, uint32_t tolen);
    int close(int s);
  ]]
  sock_lib = ffi.C
else
  ffi.cdef [[
    typedef struct { uint16_t family; uint16_t port; uint32_t addr; char zero[8]; } dd_sockaddr_in;
    int socket(int af, int type, int protocol);
    intptr_t sendto(int s, const void *buf, size_t len, int flags, const void *to, uint32_t tolen);
    int close(int s);
  ]]
  sock_lib = ffi.C
end

local AF_INET, SOCK_DGRAM, IPPROTO_UDP = 2, 2, 17

local function send(cmd)
  if sock == nil then
    sock = sock_lib.socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP)
  end
  local addr = ffi.new("dd_sockaddr_in")
  if ffi.os == "OSX" then addr.len = ffi.sizeof(addr) end
  addr.family = AF_INET
  addr.port = sock_lib.htons(port)
  addr.addr = sock_lib.inet_addr(host)
  local r = sock_lib.sendto(sock, cmd, #cmd, 0, addr, ffi.sizeof(addr))
  if tonumber(r) < 0 then
    obs.script_log(obs.LOG_WARNING, "falha ao enviar comando '" .. cmd .. "' para " .. host .. ":" .. port)
  end
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

local function start_relay()
  if relay_path == "" then return end
  if is_windows then
    local dir = relay_path:match("^(.*)[\\/]") or nil
    -- 7 = SW_SHOWMINNOACTIVE: console window starts minimized
    shell32.ShellExecuteA(nil, "open", relay_path, nil, dir, 7)
  else
    os.execute('"' .. relay_path .. '" > /dev/null 2>&1 &')
  end
end

---------------------------------------------------------------------------
-- Hotkeys
---------------------------------------------------------------------------
local actions = {
  { id = "dyn_delay_toggle", desc = "Delay dinâmico: ligar/desligar", cmd = function() return "toggle" end },
  { id = "dyn_delay_on", desc = "Delay dinâmico: ligar", cmd = function() return "on" end },
  { id = "dyn_delay_off", desc = "Delay dinâmico: desligar (voltar ao vivo)", cmd = function() return "off" end },
  { id = "dyn_delay_plus", desc = "Delay dinâmico: aumentar", cmd = function() return "add " .. step end },
  { id = "dyn_delay_minus", desc = "Delay dinâmico: diminuir", cmd = function() return "add -" .. step end },
}

function script_description()
  return [[<h3>Delay dinâmico</h3>
<p>Liga e desliga o delay da live sem parar a transmissão.
Requer o <b>obs-dynamic-delay</b> rodando e o OBS transmitindo para
<code>rtmp://127.0.0.1:1935/live</code>.</p>
<p>Atalhos em <i>Configurações &gt; Atalhos &gt; Delay dinâmico</i>.</p>]]
end

function script_properties()
  local p = obs.obs_properties_create()
  obs.obs_properties_add_button(p, "btn_toggle", "Ligar/desligar delay agora", function() send("toggle"); return false end)
  obs.obs_properties_add_button(p, "btn_panel", "Abrir painel de controle", function() open_target(panel_url); return false end)
  obs.obs_properties_add_int(p, "step", "Passo do aumentar/diminuir (s)", 1, 120, 1)
  obs.obs_properties_add_path(p, "relay_path", "Executável do relay", obs.OBS_PATH_FILE,
    "Executável (*.exe);;Todos (*.*)", nil)
  obs.obs_properties_add_bool(p, "autostart", "Iniciar o relay junto com o OBS")
  obs.obs_properties_add_button(p, "btn_start", "Iniciar relay agora", function() start_relay(); return false end)
  obs.obs_properties_add_text(p, "host", "Host do relay", obs.OBS_TEXT_DEFAULT)
  obs.obs_properties_add_int(p, "port", "Porta UDP do relay", 1, 65535, 1)
  obs.obs_properties_add_text(p, "panel_url", "URL do painel", obs.OBS_TEXT_DEFAULT)
  return p
end

function script_defaults(s)
  obs.obs_data_set_default_string(s, "host", "127.0.0.1")
  obs.obs_data_set_default_int(s, "port", 8788)
  obs.obs_data_set_default_int(s, "step", 5)
  obs.obs_data_set_default_string(s, "panel_url", "http://127.0.0.1:8787/")
  obs.obs_data_set_default_bool(s, "autostart", false)
end

function script_update(s)
  host = obs.obs_data_get_string(s, "host")
  port = obs.obs_data_get_int(s, "port")
  step = obs.obs_data_get_int(s, "step")
  panel_url = obs.obs_data_get_string(s, "panel_url")
  relay_path = obs.obs_data_get_string(s, "relay_path")
  autostart = obs.obs_data_get_bool(s, "autostart")
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
  if autostart then start_relay() end
end

function script_save(s)
  for _, a in ipairs(actions) do
    local arr = obs.obs_hotkey_save(a.hk)
    obs.obs_data_set_array(s, a.id, arr)
    obs.obs_data_array_release(arr)
  end
end

function script_unload()
  if sock ~= nil then
    if is_windows then sock_lib.closesocket(sock) else sock_lib.close(sock) end
    sock = nil
  end
end
