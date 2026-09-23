# obs-dynamic-delay

Delay dinâmico para lives no OBS: o streamer **liga e desliga o delay quando quiser, com a live no ar**, sem derrubar a transmissão.

O "Stream Delay" nativo do OBS (Configurações > Avançado) só pode ser alterado com a live parada. Este projeto resolve isso com um relay local escrito em Rust que fica entre o OBS e a plataforma:

```
OBS ──RTMP──▶ obs-dynamic-delay (127.0.0.1:1935) ──RTMP/RTMPS──▶ Twitch / YouTube / Kick ...
                    ▲            ▲
          hotkeys (script Lua)   painel web / API HTTP (dock no OBS, Stream Deck)
```

## O que o público vê

| Ação | Efeito para quem assiste |
|---|---|
| **Ligar o delay** (ex.: 30 s) | A imagem congela no próximo keyframe (com áudio mudo) durante 30 s, enquanto o buffer enche. Depois a live continua normalmente, 30 s atrasada. A conexão com a plataforma nunca cai. |
| **Desligar o delay** | Corte seco para o presente: o trecho em buffer é descartado e a live volta a ficar ao vivo (com precisão de ~1 GOP, 2 s no padrão do OBS). |
| **Aumentar / diminuir** (±5 s, presets) | Mesma lógica: aumentar congela pela diferença, diminuir corta pela diferença. |
| **Parar a live no OBS com delay ligado** | O relay termina de enviar o trecho atrasado e só então encerra a live na plataforma. Para encerrar na hora, desligue o delay depois de parar. |

Nada é re-encodado: o relay só guarda e reenvia os pacotes que o OBS já codificou (CPU quase zero), reescrevendo os timestamps para a plataforma receber uma linha do tempo contínua. A memória usada é mais ou menos o bitrate vezes o delay (6 Mbps × 30 s ≈ 23 MB).

## Instalação

### 1. Relay

Baixe o `obs-dynamic-delay.exe` na aba Releases, ou compile:

```sh
cargo build --release   # gera target/release/obs-dynamic-delay.exe
```

Na primeira execução ele cria um `config.toml` ao lado do executável:

```toml
listen = "127.0.0.1:1935"                     # onde o OBS se conecta
upstream_url = "rtmp://live.twitch.tv/app"    # destino real (YouTube: rtmp://a.rtmp.youtube.com/live2)
stream_key = ""                               # vazio = usa a chave digitada no OBS
delay_seconds = 30                            # delay aplicado ao ligar
start_enabled = false                         # começar a live já com delay
max_delay_seconds = 600
filler_fps = 2                                # fps da imagem congelada
http_listen = "127.0.0.1:8787"                # painel / API
udp_listen = "127.0.0.1:8788"                 # comandos do script do OBS
```

`rtmps://` também funciona (Kick e outras plataformas que exigem TLS).

### 2. OBS

1. **Configurações > Transmissão**: Serviço **Personalizado...**, Servidor `rtmp://127.0.0.1:1935/live`, Chave = sua chave real (ou qualquer coisa, se ela estiver no `config.toml`).
2. **Configurações > Avançado > Stream Delay**: deixe **desligado** (quem cuida do delay agora é o relay).
3. **Ferramentas > Scripts > +**: adicione `obs/obs-dynamic-delay.lua`. Não precisa instalar Python, o OBS já traz o LuaJIT.
   - Opcional: aponte o campo "Executável do relay" para o `.exe` e marque "Iniciar o relay junto com o OBS".
4. **Configurações > Atalhos**: procure "Delay dinâmico" e defina as teclas:
   - ligar/desligar · ligar · desligar (voltar ao vivo) · aumentar · diminuir
5. **Docks > Docks de navegador personalizados**: nome "Delay", URL `http://127.0.0.1:8787/`. O dock mostra o estado (AO VIVO / AJUSTANDO / DELAY), o atraso real em segundos e os botões.

## API

Aceita GET e POST, então funciona direto no Stream Deck (ação "Website"), no Touch Portal, em bots etc.

| Rota | Ação |
|---|---|
| `/api/toggle` | liga/desliga |
| `/api/on` · `/api/off` | liga · desliga |
| `/api/delay/{s}` | define o delay em segundos |
| `/api/add/{s}` | soma (aceita negativo: `/api/add/-5`) |
| `/api/status` | estado em JSON |

A porta UDP (`8788`) aceita os mesmos comandos em texto: `toggle`, `on`, `off`, `set 30`, `add -5`.

## Por que Rust e não Python

O trabalho pesado é um servidor + cliente RTMP que precisa segurar minutos de vídeo em memória e reenviar tudo no tempo certo, por horas, sem engasgar. Rust entrega isso num único `.exe` sem dependências, com uso de CPU desprezível e sem pausas de GC. Um script Python dentro do OBS não consegue interceptar o stream codificado, e exigiria que cada streamer configurasse um interpretador Python compatível com a versão do OBS. A parte que precisa rodar dentro do OBS (os atalhos) ficou em Lua, que já vem embutido no OBS.

## Limitações

- Só RTMP/RTMPS (o padrão das plataformas). WHIP/SRT não passam pelo relay.
- Ligar o delay espera o próximo keyframe (até 2 s com o intervalo padrão), e desligar corta num keyframe.
- A imagem congelada repete um keyframe; com `filler_fps = 2` isso usa pouca banda. Se a plataforma reclamar de fps baixo, suba o valor.
- Testado de ponta a ponta localmente (ffmpeg como OBS e como plataforma, H.264 com B-frames + AAC). Antes de usar numa live importante, faça um teste numa live não listada na sua plataforma.

## Desenvolvimento

```sh
cargo test                  # testes do motor de delay, parser FLV, config
bash scripts/e2e-test.sh    # ponta a ponta com ffmpeg (precisa de ffmpeg e curl no PATH)
```

Estrutura:

- `src/engine.rs`: motor de delay (buffer, congelamento, corte, reescrita de timestamps)
- `src/ingest.rs`: servidor RTMP que recebe do OBS
- `src/upstream.rs`: cliente RTMP/RTMPS que publica na plataforma (reconecta sozinho)
- `src/control.rs` + `src/panel.html`: API HTTP, painel e porta UDP
- `obs/obs-dynamic-delay.lua`: script do OBS (atalhos, botão, auto-início do relay)
