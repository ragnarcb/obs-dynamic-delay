# First real stream checklist / Checklist da primeira live real

**English** · [Português](#português)

Use an unlisted or test stream (YouTube "unlisted", Twitch with a test title) and watch it on your phone next to the PC.

1. **Install** with OBS closed, open OBS, and check the **Dynamic Delay** panel shows *OBS ready* and no *stream key missing* warning.
2. **Start streaming** in OBS. In **Stream health**: *OBS streaming*, your platform *connected*, bitrate above 0.
3. **Watch on the phone** until the picture shows up.
4. **Turn the delay on** (30 s). The panel goes ADJUSTING and then DELAY 30.0 s. On the phone, with the default rewind mode, the stream jumps back and keeps playing.
5. **Turn it off.** The panel goes LIVE; on the phone the stream cuts to the present.
6. With the delay on, press **Delete before it airs**. The panel shows *Deleted the last ...s*; on the phone, about 30 s later, the stream holds a frame and skips that part.
7. **Instant replay** and **Save clip**: the clip appears in `Videos\Dynamic Delay` and plays in any player.
8. If you use them: **panic button** (scene + mute and back), a **scene rule**, a **chat command** from a mod, the **phone QR code**, the **Stream Deck**.
9. **Multistream:** add a second destination, start a new stream, check both show *connected*.
10. **Stop streaming** with the delay on: the platform ends the stream about 30 s later, after the delayed tail.

Something off? Open an issue with `%APPDATA%\obs-dynamic-delay\obs-dynamic-delay.log` and what the panel showed.

---

## Português

Use uma live de teste ou não listada (YouTube "não listado", Twitch com título de teste) e assista no celular ao lado do PC.

1. **Instale** com o OBS fechado, abra o OBS e confira se o painel **Delay dinâmico** mostra *OBS pronto* e nenhum aviso de *falta a chave*.
2. **Inicie a transmissão** no OBS. Em **Saúde da live**: *OBS transmitindo*, sua plataforma *conectada*, bitrate acima de 0.
3. **Assista no celular** até a imagem aparecer.
4. **Ligue o delay** (30 s). O painel passa por AJUSTANDO e depois DELAY 30,0 s. No celular, com o modo padrão (rebobinar), a live volta no tempo e continua.
5. **Desligue.** O painel volta para AO VIVO; no celular a live corta para o presente.
6. Com o delay ligado, aperte **Apagar antes de ir ao ar**. O painel mostra *Apagados os últimos ...s*; no celular, uns 30 s depois, a live segura um quadro e pula aquele trecho.
7. **Replay instantâneo** e **Salvar clipe**: o clipe aparece em `Vídeos\Dynamic Delay` e abre em qualquer player.
8. Se for usar: **botão de pânico** (cena + mudo e volta), uma **regra por cena**, um **comando no chat** vindo de um mod, o **QR code do celular**, o **Stream Deck**.
9. **Multistream:** adicione um segundo destino, comece uma live nova e confira os dois *conectados*.
10. **Pare a transmissão** com o delay ligado: a plataforma encerra uns 30 s depois, após o trecho atrasado.

Algo estranho? Abra uma issue com o `%APPDATA%\obs-dynamic-delay\obs-dynamic-delay.log` e o que o painel mostrou.
