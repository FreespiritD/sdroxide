# SDR Oxide

A PowerSDR/Thetis-style software-defined-radio transceiver client in Rust, with
pluggable radio backends (**SoapySDR**, **OpenHPSDR**, **TCI**, **SmartSDR**, and **CAT**), an
[egui](https://github.com/emilk/egui) GUI, and a cyberpunk theme. It runs as a **native desktop application** and, from the same
binary, as a **server that streams the same UI to a web browser** over
WebSocket. It includes an integrated, persistent **logbook**, many digital modes like **FT8/FT4/FT2**
built-in, and **TCI and Hamlib rigctld servers** so third-party programs like WSJT-X can use it as their radio.

<hr/>

<img width="1496" height="933" alt="image" src="https://github.com/user-attachments/assets/9d88118c-0efe-45c5-9918-8ee2bb91b700" />

<hr/>

<img width="1682" height="1212" alt="image" src="https://github.com/user-attachments/assets/aa08f5d3-ec62-4d91-9dd0-13bde1b0ae43" />

<hr/>

<img width="1496" height="933" alt="image" src="https://github.com/user-attachments/assets/902a73ff-c8bf-43cd-9fc3-884d40ce4b04" />

<hr/>

> ## [User Manual](docs/USER_MANUAL.md)

One binary, three ways to run it:

- **Native** — a local desktop transceiver against your SDR hardware.
- **Server** — `sdroxide --server`; the DSP runs on the machine with the radio
  and the full UI (plus audio and the waterfall) is served to a browser as
  WebAssembly. One remote client at a time.
- **Native remote** — `sdroxide --connect host:4950`; the desktop UI driving a
  remote server instead of local hardware.

## Core features

- **Radios** - CAT/Audio, CAT/Stereo IQ, TCI (SunSDR), OpenHPSDR P1 and P2
  (Hermes Lite 2, Apache Labs), SoapySDR (HackRF, etc.), RTL-SDR (native support), 
  RX-888 (native support), SDRplay RSP (native, via the vendor API service),
  SmartSDR (FlexRadio - experimental!), PlutoSDR (native support, experimental!)
- **Panadapter** — GPU (wgpu) waterfall + spectrum line, wheel-zoom around the
  cursor, drag-to-pan, per-digit frequency readout, selectable colormaps,
  peak-hold, and a **one-click auto-contrast** ("FIT") that picks the display
  floor/ceiling from the signals currently on screen.
- **Modes** — SSB (USB/LSB), CW, AM, SAM, NFM (with **CTCSS/DCS** decoding and
  tone squelch), WFM (with broadcast
  pilot-tone **stereo**), DSB, DIGU/DIGL, a
  spectrum-only mode, **FT8/FT4/FT2**, **JS8** (all four speeds, with directed
  messaging, heartbeats and multi-frame free text), the keyboard modes
  **PSK31**, **RTTY**,
  **Olivia**, **THOR** and **FSQ** (with directed messaging + images),
  **WSPR** (transmit and receive, with WSPRnet reporting and optional band
  hopping),
  **Hellschreiber** (all seven Feld Hell / FSK Hell variants, on a scrolling
  raster), image **SSTV** (Scottie, Martin, Robot), image **RIFP**
  (draft-dulaunoy-rifp-00 — packetised, checksummed pictures over a 4800-baud
  CPFSK modem), receive-only **weather fax** (WEFAX/radiofax charts with a
  station picker, phasing and slant correction), and transmit-only **RF Paint**
  (spectrum painting of text and images onto the waterfall).
- **Receiver** — hang AGC, draggable passband filter edges (on the spectrum and
  the waterfall), noise blanker, auto-notch, **four noise-reduction engines**
  (RNNoise, DeepFilterNet3, a libspecbleach port and the built-in spectral NR,
  three strengths each), squelch, a second sub-receiver, RIT/XIT, VFO A/B with split,
  per-band band stacks, and memory channels.
- **Bandplan overlay** — a colour-coded strip along the bottom of the waterfall
  that labels allocations (ham bands, broadcast, CB, AM); it shows coarse bands
  when zoomed out and CW/digital/SSB sub-segments when zoomed into a ham band.
- **Scanner** — work through the memory channels or a frequency range and stop
  where somebody is transmitting, with a configurable threshold (or the
  receiver's own squelch), dwell, skip list, and carrier / timed / manual
  resume. A range scan reads a whole span out of the panadapter's FFT rather
  than visiting channels one at a time, so sweeping 2 m takes well under a
  second instead of minutes.
- **Transmit** — PTT and tune carrier, drive/ALC metering, device-aware
  half-duplex sequencing (HackRF) or full-duplex (LimeSDR), and a ham-band /
  TX-range lockout so you can't key outside your allocation.
- **Resizable layout** — drag the frequency-scale strip to resize the spectrum
  vs. waterfall split; in FT8/FT4/FT2, drag the divider to resize the operating
  panel.
- **Live spotting, awards & QSL** — DX cluster / POTA / SOTA / PSK Reporter spots
  as clickable panadapter markers (click to tune + pre-fill a log entry),
  QRZ/HamQTH callsign lookup, one-click upload to LoTW / eQSL / Club Log / QRZ,
  and live **DXCC / WAS / WAZ / grid** award tracking (worked vs confirmed).
- **Control inputs** — every shortcut is rebindable, and any class-compliant
  **MIDI controller** can drive the radio: a jog wheel as the VFO knob, pads as
  PTT and band buttons, faders as gain controls, with LED/motor feedback. Mouse
  buttons take bindings too (a side button held for PTT works as a footswitch),
  and the panadapter wheel can zoom or tune.
- **Spoken announcements** — the radio reads itself out, for operating it
  without seeing it. A neural voice ships with the program and runs locally, so
  nothing is sent anywhere and no speech service has to be installed. It reads
  the frequency once the dial stops moving, folds a band change into one phrase,
  warns when you leave an amateur band, reads the SWR out while you tune up
  (with a warning above 3:1), and announces FT8/JS8 messages addressed to you.
  Announcements play on their own sound device, so they are never recorded and
  never sent to a remote listener. The window is also exposed to NVDA, Orca and
  VoiceOver.
- **Persistence** — device, rates, gains, memories, band stacks, the FT8/FT4/FT2
  operator profile, network/QSL credentials, control bindings, and the logbook
  are all stored under `~/.config/sdroxide/`.

## FT8 / FT4 / FT2

Selecting FT8, FT4 or FT2 switches the panadapter to a zoomed sub-band waterfall
with a decode list and an auto-sequencing QSO panel. The three are one protocol
at three speeds — 15 s slots for FT8, 7.5 s for FT4, 3.75 s for FT2 — sharing a
message format, a panel and a logbook:

- Click a decoded line to move your TX audio frequency onto that signal (a faint
  marker appears on the world map); press **REPLY** to start an auto-sequenced
  QSO, or **Call CQ** to call.
- A dot-matrix **world map** shows your grid, the station you're working, and an
  animated pulse travelling the great-circle path while you transmit.
- Own callsign, grid, and message templates are set in the FT8/FT4/FT2 setup dialog
  and persisted.
- **FT2** trades sensitivity and spectrum for speed: 4-GFSK at 41.667 baud,
  167 Hz wide, a 2.52 s burst, and a complete contact in about six seconds. It
  wants an accurate clock — its timing search is only about half a second wide.
- All decoding and encoding run server-side in the native engine, so native and
  browser clients behave identically.

## WSPR

Selecting **WSPR** opens a reception list beside the world map, and a beacon
status pane. WSPR is not a QSO mode — a transmission carries a callsign, a grid
and a power level and nothing else — so what the panel shows is measurements of
paths rather than a conversation.

- **Receive** decodes every two-minute slot. Each row is a beacon heard (`←`) or,
  once "who heard me" is on, a station that heard *this* one (`→`), with its
  locator, signal report, declared power and distance. Reports are coloured on
  WSPR's own scale, where −25 dB is still a good path.
- **Transmit** is off until you ask for it. The panel carries the duty cycle
  (10–50% of slots) and the power you actually radiate, in watts — only the
  nineteen levels WSPR's message can name are offered, because that figure goes
  out on the air and everyone who hears you judges the path by it. The beacon
  picks its slots from your callsign, so two stations running sdroxide do not
  transmit on top of each other, and it moves within the 200 Hz window each
  time. Callsign and grid come from Settings → General, like everywhere else.
- **Band hopping** moves the dial between slots so one receiver samples the whole
  spectrum. Turning the VFO yourself pauses it and says so.
- **WSPRnet** — spots are uploaded as they are decoded (this is on by default; it
  puts nothing on the air and it is what makes a receiver part of the network),
  and **WHO HEARD ME** polls wsprnet.org for reports of your own callsign, which
  is the only feedback a beacon ever gets.
- Transmitting needs a plain callsign and a 4-character locator — the 50-bit
  message has room for nothing else. A compound call or a 6-character grid is
  said plainly rather than mangled; receiving is unaffected.

## Propagation heat map

Everything the station hears becomes evidence about the ionosphere, and the
**PROP** layer on the 3D globe draws it: WSPR both ways, FT8/FT4/FT2 and JS8
decodes, and the logbook.

- Each reception is placed at the **midpoint of its path** — the patch of
  ionosphere that bent it — rather than at the far station, so the picture is a
  map of the sky rather than of where radio amateurs live. Long paths get a
  control point per hop.
- **ALL BANDS** gives every band its own hue, **ONE BAND** runs a
  single band through a blue → green → yellow → red ramp. The same picture can be switched on under the flat map in the operating panel.

## PSK31 and RTTY

Selecting **PSK** or **RTTY** opens a live keyboard-mode ragchew panel next to a
zoomed sub-band waterfall — tune onto a signal, watch it decode, and type a
reply that transmits as you type:

- **Receive** streams decoded text into a scrolling window. Fine-tune with the
  **−/+** buttons (±10 Hz) onto the carrier; RTTY draws mark/space tuning lines
  on the waterfall.
- **Transmit** as you type: characters already sent turn **green** so you can
  watch the transmission catch up to your typing. **TX** keys/unkeys, **CALL CQ**
  loads and sends a CQ macro, **CLEAR** empties the buffer.
- **PSK** is BPSK31 (differential BPSK, varicode). **RTTY** defaults to 45.45
  baud / 170 Hz shift / Baudot; shift (170/425/850 Hz) and baud (45/50/75) are
  selectable in the PSK/RTTY setup dialog.
- The **PSK and RTTY skimmers** decode signals across each band's PSK/RTTY
  calling sub-bands and label them on the waterfall; click a label to switch to
  that mode, tune onto it, and open the panel.

## Olivia, THOR and FSQ

Three more keyboard modes share the same ragchew panel and setup dialog as
PSK/RTTY; the submode is chosen on each mode's setup page:

- **Olivia** — a very robust MFSK chat mode with Walsh/Hadamard coding. Pick the
  tone count (2–64) and bandwidth (125–2000 Hz); 32/1000 and 16/500 are common.
- **THOR** — DominoEX-family 18-tone incremental-FSK with convolutional FEC.
  Pick a submode (THOR4 … THOR32; THOR16 is the usual default).
- **FSQ** — 33-tone incremental-FSK (speeds FSQ-2/3/4.5/6) with a dedicated panel
  for the **directed (FSQCALL)** layer: a **heard list**, a persistent **contacts**
  book, directed `CALL:message` sends, ALLCALL broadcast, an automatic reply to
  the `?` heard-list query, and **image** transmit/receive (pick a picture to send;
  received pictures land in the gallery).

These modems are native-Rust and self-contained (no external decoder); on-air
interoperability with fldigi is being validated and refined.

## SSTV

Selecting **SSTV** opens an image panel with a received-image gallery on the
left and a transmit compositor on the right:

- **Receive** decodes incoming pictures scanline-by-scanline into the gallery;
  the VIS header sets the mode automatically (and pre-selects it for your next
  transmit). Received images are saved under `~/.config/sdroxide/sstv_rx/`.
- **Transmit** from a strip of five image slots — click to select, double-click
  (or click an empty slot) to pick a file, which is auto-cropped/scaled to the
  mode's size. A multi-line message is overlaid on the image, **each line in a
  different font**, bold with a black outline; a live preview shows exactly what
  will be sent. Every transmitted image carries a small red→black header strip
  with "SDRoxide" and the version. **TX** sends; **ABORT TX** stops.
- **Modes:** Scottie 1 / 2 / DX, Martin 1 / 2, Robot 72, Robot 36. Band buttons
  tune to that band's SSTV calling frequency (e.g. 20 m = 14.230 MHz).

## RIFP

Selecting **RIFP** opens the same image panel over the **Radio Image Framing
Protocol** ([draft-dulaunoy-rifp-00](https://datatracker.ietf.org/doc/draft-dulaunoy-rifp/)):
a picture is encoded, split into numbered chunks, and sent as CRC-protected
frames behind a JSON manifest, with the complete object verified by CRC-32 and
SHA-256 before it is shown. Interoperates both ways with the
[reference implementation](https://github.com/adulau/rifp) across every encoding
either side can produce.

- **Radio profile** `rifp-cpfsk-4800`: continuous-phase binary FSK, 4800 baud,
  ±4 kHz, sent on the carrier rather than in a sideband — **the dial is the
  centre of the signal**. ⚠ Its ~25 kHz channel does not fit a narrow-band
  segment; the panel warns wherever it does not, and band buttons land in the
  segments where it does — 10 m FM, the 6 m and 2 m all-modes parts, and 70 cm,
  where a **433.920** chip jumps to the calling frequency the draft names.
- **Encodings:** CCITT Group 4 facsimile, PNG, JPEG, and the packed grayscale
  raster raw / RLE8 / ZLIB — or Auto, which sends whichever comes out smallest.
  1, 2, 4 or 8 bits per pixel, with optional dithering.
- **Receive** shows every transfer being reassembled with a per-chunk map of
  what has arrived, paints the raw raster row by row as it lands, and adds a
  picture to the gallery only once its digest checks out.

## RF Paint

Selecting **RFPAINT** opens a transmit-only **spectrum-painting** panel that draws
text and pictures **directly onto a receiver's waterfall** — there is no decoder,
the picture *is* the signal, so anyone watching their panadapter on your frequency
simply sees what you paint. It transmits on USB inside a 3 kHz audio band, so it
fits a normal SSB channel:

- **Text paint** — type a line and it is rendered as upright letters that scroll
  up the far station's waterfall (constant font size — a longer message just makes
  a wider banner / longer transmission).
- **Image paint** — load a PNG/JPEG, reduced to a contrast-stretched grayscale
  bitmap and painted onto the waterfall.
- Each area has a **live preview waterfall** showing exactly how it will look on
  the receiving end, plus a **TRANSMIT** button, a transmit-progress bar, and
  **Abort**.
- A **scan-speed** control (≈6%–100%, default 25%) trades transmission time for
  legibility — slower gives the receiver's waterfall more scan lines to render
  the picture. Transmit goes through the normal path, so the ham-band lockout and
  transmit safety still apply.

## RADE digital voice

Selecting **RADE** switches the receiver to **FreeDV RADE V1** (Radio
Autoencoder) — a neural speech codec carried on an OFDM waveform, which stays
intelligible at signal-to-noise ratios where SSB is just noise. It fits inside a
normal USB channel, occupying roughly 1060–1880 Hz of audio.

- **Receive** replaces the demodulated audio with the decoded speech as soon as
  the modem locks. Out of sync you still hear the raw signal, so you can tune by
  ear; the panel shows a sync lamp, the SNR estimate and the frequency offset,
  and the waterfall is marked with the band the waveform occupies.
- **Transmit** with the panel's **TALK** button or the ordinary PTT. The modem 
  needs ~120 ms of speech before the first frame goes out and appends an 
  end-of-over frame when you stop, so transmit runs on slightly past the button.
- Band buttons tune to the FreeDV calling frequencies (e.g. 20 m = 14.236 MHz).
- Decoding is neural-network inference and runs on its own thread; it is far
  faster than real time on a modern CPU, but the panel warns if the machine
  falls behind.

`rade-harness` (in `crates/sdroxide-rade`) drives the same codec over files, for
bench testing without a radio:

```sh
cargo run -p sdroxide-rade --bin rade-harness -- \
    tx --input vendor/rade_c/wav/david_vk5dgr.wav --output modem8k.wav
cargo run -p sdroxide-rade --bin rade-harness -- \
    rx --input modem8k.wav --speech decoded16k.wav --stats rx.csv
```

## Logbook

Open the **LOG** button (available in any mode) for a persistent logbook that
holds both FT8/FT4/FT2 and manually entered QSOs:

- Entries are grouped into daily **sessions** with a time span and QSO count.
- **+ New Entry** adds a manual QSO. Alongside the basics (call, frequency, mode,
  RST, grid, UTC date/time) the entry form now carries **name, QTH, state,
  country**, transmit **power**, and **contest** fields (contest id + sent/received
  serials); a **worked-before** badge warns when you've already worked that call
  on the band. **EDIT** and **DEL** amend or remove any past entry.
- FT8/FT4/FT2 QSOs are logged automatically as they complete.
- **IMPORT** loads QSOs from an ADIF (`.adi`) file (de-duplicated against the
  existing log); export the whole book to **ADIF** or plain **TXT**. A
  QSL/confirmation status column shows what's been uploaded and confirmed.
- Records also hold DXCC entity, CQ/ITU zones, IOTA and POTA/SOTA references, and
  per-service QSL status — the data behind lookup, upload and award tracking.
- The log is stored at `~/.config/sdroxide/qso_log.json` (native) or in browser
  storage (remote).

## Spotting, awards & QSL upload

Turn the logbook into a live station cockpit. Everything here is configured on
the **Spots** and **Uploads** tabs of the Settings dialog, and surfaced by the
**SPOTS** and **AWARDS** buttons in the System module.

![Live spots as clickable markers on the panadapter, and the SPOTS window](docs/images/14-spots-panel.png)

- **Spot feeds** — connect a **DX cluster** (telnet) and poll **POTA**, **SOTA**
  and **PSK Reporter**. Spots appear as clickable, colour-coded markers along the
  bottom of the waterfall (and as dots on the FT8 world map); the **SPOTS** window
  lists them with per-source filters and a **fuzzy search** over calls, station
  names, sites and frequencies. **Click a spot** to tune the VFO, set the mode,
  and pre-fill a new log entry — one click from "heard" to "working".
- **Broadcast stations** — ~4,600 **longwave and shortwave broadcasts** label the
  AM carriers on the waterfall. Each carries its **UTC transmit window** and the transmitter
  site it actually radiates from, so only the stations on air right now are
  shown, and tuning one draws a great-circle arc from your grid to the
  transmitter on the 3D globe. The schedule is downloaded from
  [EiBi](https://www.eibispace.de/) on first run and again at each season change,
  falling back to a built-in copy when offline. Users can define their own stations and corrections
  in `~/.config/sdroxide/broadcast_stations.json`.
- **Callsign lookup** — auto-fill name, QTH, grid and state from **QRZ.com** or
  **HamQTH** on a spot click, at QSO start, or when you type a call (or press
  **LOOKUP** in the entry form).
- **One-click upload** — push QSOs to **eQSL**, **QRZ Logbook** and **Club Log**
  (a per-QSO **UP** button, or automatically as each QSO is logged). **LoTW** is
  handled by exporting ADIF for TQSL signing; LoTW/eQSL **confirmations are
  downloaded** to mark worked-vs-confirmed.
- **Award tracking** — the **AWARDS** window tallies **DXCC**, **WAS**, **WAZ**
  and **grid squares**, worked vs confirmed, with a per-band filter. DXCC entity
  and CQ/ITU zones are resolved from the callsign (bundled `cty.dat`), so spots
  for a **new entity** are flagged in the SPOTS list.

Credentials are stored in plaintext under `~/.config/sdroxide/net.json` (as with
other ham software). See the [User Manual](docs/USER_MANUAL.md) for setup steps.

## Radio backends

sdroxide can drive eight kinds of radio, selected on the **Radio** tab of the
Settings window. Backend, serial, and radio-audio changes apply live when you
press **Apply / reconnect**. A radio that isn't there yet at startup — or that
drops mid-session — is retried in the background and attaches by itself, so
starting sdroxide before the rig is fine:

- **RTL-SDR (USB)** — an RTL2832U dongle, driven directly over USB by a native
  pure-Rust driver. **No SoapySDR and no libusb needed**, so it works in every
  build including the standard `.msi` and `.dmg`. Covers the R820T, R820T2 and
  R828D tuners, which is effectively every dongle still sold. HF works through
  an RTL-SDR Blog V4's built-in upconverter, or on other sticks by direct
  sampling the ADC's Q branch (the V3's HF port). Bias tee and ppm correction
  are on the Radio tab; see "RTL-SDR permissions" under Building.
- **RX-888 (USB)** — an RX-888 or RX-888 Mk2 direct-sampling HF receiver
  (LTC2208 16-bit ADC, Cypress FX3), driven directly over USB by a native
  pure-Rust driver. **No SoapySDR, no libusb, and no vendor driver package.**
  Covers 0–32 MHz by sampling HF directly at up to 129.6 Msps.

  The FX3 on this board has no boot EEPROM, so the receiver appears as a bare
  Cypress bootloader on every plug-in with no radio function at all. sdroxide
  uploads the (MIT-licensed, bundled) firmware itself, so there is nothing to
  install and nothing to run first — plug it in and pick it in Settings. See
  "RX-888 permissions" under Building for the Linux udev rule.

  There is no hardware downconverter in this receiver: the full ADC stream is
  converted to complex baseband on the host, which is why retuning anywhere in
  HF is instantaneous, and why it wants a modern CPU and a real USB 3 port.
  Receive only; the VHF/UHF tuner is not driven.
- **SDRplay RSP (USB)** — any SDRplay RSP (RSP1, RSP1A, RSP1B, RSP2, RSPduo,
  RSPdx, RSPdx R2), driven natively through the vendor's **SDRplay API
  service** — no SoapySDR in the path. The RSPs after the original RSP1 have
  no open USB protocol, so this is the one backend that needs a vendor
  package: install the [SDRplay API](https://www.sdrplay.com/api/) (v3.x) and
  make sure its service is running (Linux: `sudo systemctl enable --now
  sdrplay`). Nothing is linked at build time — sdroxide finds the library at
  runtime, so every build variant has the backend and simply reports what to
  install when it is missing (`sdroxide --probe` says which piece is absent).

  Receive only, 1 kHz–2 GHz, up to 10 Msps. The RSP gain model is exposed the
  way the hardware means it: an **IF gain reduction** slider, an **LNA state**
  step control (clamped per band, honestly reported back), and the RSP's own
  hardware **AGC** with an adjustable set point. FM-broadcast and DAB notch
  filters, bias tee, RSP2/RSPdx antenna selection, RSPduo tuner selection and
  RSPdx HDR mode are available on the Radio tab, and only the rows the selected
  model actually supports are shown.
- **PlutoSDR (network)** — an ADALM-Pluto, driven directly over the **IIOD**
  protocol its on-board daemon serves. **No SoapySDR and no libiio**, so it
  works in every build including the standard `.msi` and `.dmg`. Wideband IQ
  receive *and* transmit.

  A Pluto is a network device even on a USB cable — the cable presents an
  Ethernet gadget — so it is reached at an address (`192.168.2.1` out of the
  box) rather than by a serial number, and one on the LAN works the same way.
  Press **Discover** to ask the network, or type the address; **Test
  connection** reports what the board says about itself.

  The AD9361's four AGC modes, receive gain, transmit attenuation and both RF
  ports are on the Radio tab. Tuning limits are read off the device, so a stock
  AD9363 board (325 MHz–3.8 GHz) and one unlocked to AD9364 (70 MHz–6 GHz) are
  both reported correctly without a setting. Half duplex: receive stops for the
  length of an over, because a USB 2.0 gadget will not carry a
  megasample-per-second stream both ways at once. Not yet hardware-verified —
  see the user manual, §5.2.7.
- **SoapySDR** — any [SoapySDR](https://github.com/pothosware/SoapySDR) device
  (wideband IQ) — HackRF, Airspy, LimeSDR and friends. See below.
- **OpenHPSDR** — Hermes/Metis-family Ethernet SDRs on the LAN (Protocol 1 and
  2). Press **Discover** to scan for devices, or enter the IP manually; pick a
  DDC sample rate (48 kHz–1536 kHz). Not yet hardware-verified — testers can run
  `RUST_LOG=sdroxide_hpsdr=debug sdroxide` for connection/RX diagnostics (see the
  user manual, §5.4).
- **CAT / Audio** — a CAT-controlled rig (Icom/CI-V, Yaesu, Xiegu) with audio
  over a USB sound card, as either demodulated mono audio or stereo IQ.
- **TCI** — a TCI (Transceiver Control Interface) server such as ExpertSDR3 
  over WebSocket (default `127.0.0.1:50001`): wideband IQ receive plus 
  audio transmit.
- **SmartSDR / FlexRadio** — a FLEX-6000 or FLEX-8000 on the LAN. Press
  **Discover** to listen for radios (they announce themselves), or enter an
  address for one reached over a router or VPN. Receive is a **DAX IQ** stream,
  so sdroxide does its own demodulation and the radio's slice follows the dial;
  transmit is DAX audio the radio modulates. DAX IQ tops out at **192 kHz**,
  which is this backend's widest span.

The wideband-IQ backends (RTL-SDR, RX-888, SDRplay, SoapySDR, HPSDR, TCI,
SmartSDR, PlutoSDR) drive the full panadapter, the CW/PSK/RTTY skimmers, and
internal demodulation; a CAT rig feeding demodulated audio shows only a narrow
audio-band slice. RTL-SDR, RX-888 and SDRplay are receive-only; the others can
transmit.

Whichever backend you pick, a **converter offset** on the same tab handles an
external frequency converter: an HF upconverter (Ham It Up, SpyVerter), a
transverter, or a satellite LNB. Pick one from the list or type an offset in Hz
— the same number and sign the converter's documentation and other SDR programs
use — and tune in real frequencies: with a Ham It Up you work 10.1008 MHz while
the receiver is quietly sent to 135.1008 MHz. The dial, band buttons, memories,
the logbook and every spot and upload stay on the real on-air frequency. Receive
only — transmit is switched off while a converter is set, because a converter is
not in the transmit path. Not yet verified against physical hardware.

Beside it, **RX range** and **TX range** state which frequencies the radio
covers, in MHz (`144-146, 430-440`). Leave them empty and sdroxide uses what the
device says about itself. Fill them in when the device says nothing — publishing
a tuning range is optional in SoapySDR and plenty of drivers skip it — or when
what it says is the tuner chip's range rather than the radio's. A driver that
publishes no transmit range is taken at its word rather than silenced, so a
transceiver like the SXceiver keys up out of the box; the amateur-band gate
still applies either way.

### SmartSDR Simulator

A **wire-level radio simulator** allows for the backend to be exercised end to end with no radio present.

## Built-in TCI server

sdroxide is also a **TCI server**, so TCI-capable programs — WSJT-X's SunSDR
(TCI) rig type, JTDX, MSHV, skimmers — can use it as their radio: frequency and
mode control, a wideband IQ stream, receive audio to decode, and transmit audio
to put on the air. Several clients can connect at once.

It is on by default at `127.0.0.1:50001` and configured on the **Servers** tab
of the Settings dialog, which also shows the live client count. TCI has no
authentication, so it listens on localhost only unless you change that; the
transmitter has a single owner, and keying up locally always takes it back.
Verified against WSJT-X (rig *TCI Client RX1*, PTT via CAT, TCI audio). See the
user manual, §5.6.

## Built-in Hamlib rigctld server

Most amateur software reaches a radio through **Hamlib**, over the network
protocol its `rigctld` daemon speaks. sdroxide serves that protocol directly, so
**WSJT-X, fldigi, JS8Call, N1MM, Log4OM, GPredict and CQRLOG** can drive it with
no extra daemon, no serial cable and no virtual COM port pair — frequency, mode
and passband, PTT, VFO A/B and split, RIT/XIT, power and volume levels, the
NB/NR/ANF/MUTE functions, and the VFO operations.

It is **off by default** — port 4532 is often already held by a real `rigctld`,
and the protocol has no authentication — and lives on the **Servers** tab next
to the TCI server. In WSJT-X or fldigi choose the rig **Hamlib NET rigctl**
(model 2) and point it at `127.0.0.1:4532`. Unlike TCI it carries control only,
no audio or IQ; both servers can run at once. See the user manual, §5.10.

## Control inputs

Every keyboard shortcut is a rebindable **action**, and the same action list is
reachable from mouse buttons and from a MIDI controller — the cheapest real VFO
knob there is. Configured on the **Controls** tab; see the user manual, §5.9.

Push-to-talk ships **unbound** on purpose. One click binds hold-to-talk to
Space, and a held PTT is released on key-up, on window focus loss, on a text
field taking the keyboard, when the controller is unplugged, and after a
configurable timeout.

Bindings are stored with the *user interface*, not the engine, so a knob plugged
into your laptop works against a remote radio over `--connect` too.

## SoapySDR connectivity

sdroxide talks to any [SoapySDR](https://github.com/pothosware/SoapySDR) device.
It has been developed against a **HackRF One** (half-duplex TX) and a
**LimeSDR** (full-duplex TX).

- Select a device with `--device`, using SoapySDR argument syntax, e.g.
  `--device driver=hackrf` or `--device driver=lime,serial=...`. With no
  argument it uses the configured device, else the first one found.
- `sdroxide --probe` lists all detected devices and their probed capabilities
  (frequency and sample-rate ranges, gains, antennas, sensors, duplex) and
  exits.
- Capabilities drive the UI: RX-only devices hide all TX controls, band buttons
  grey out outside the device's tunable range, and SWR/power meters appear only
  when the device exposes those sensors.
- Hardware-free sources for testing: `--siggen` (built-in signal generator) and
  `--file <raw CF32 IQ>`.

## Building

### Toolchain

Install Rust with [rustup](https://rustup.rs/) rather than your distribution's
`rust`/`cargo` package. The workspace is edition 2024, so it needs Rust 1.85 or
newer, and the browser client needs a second compilation target that only
rustup can add:

```sh
rustup target add wasm32-unknown-unknown
```

A distro-packaged cargo cannot add targets itself — some distros ship the wasm
standard library as a separate package, but the usual symptom is that the native
build works fine and the web client build fails on the missing target. Migrating
to rustup is the shortest way out.

The RADE digital-voice codec is vendored as a git submodule, so clone with:

```sh
git clone --recurse-submodules https://github.com/dividebysandwich/sdroxide
# or, in an existing checkout:
git submodule update --init --recursive
```

### What depends on what

The native binary and the browser client are two separate builds. Only one
combination couples them, and it couples them at *compile* time:

| You want | Build | Web client needed? |
| --- | --- | --- |
| Native desktop UI | `cargo build --release` | no |
| Native remote client (`--connect`) | `cargo build --release` | no |
| Server, client served from a directory | `cargo build --release`, run with `--web-root` | yes, at run time |
| Server, client baked into the binary | `cargo build --release --features embed-web` | **yes, before you compile** |

`embed-web` embeds `crates/sdroxide-web/dist` with `rust-embed`, so that
directory has to exist *while cargo compiles the server*. It is `.gitignore`d
and therefore absent from a fresh clone, so reaching for `--features embed-web`
first thing fails with:

```
#[derive(RustEmbed)] folder '.../crates/sdroxide-web/dist' does not exist
```

Build the web client first (below) and it compiles. Nothing else in the
workspace depends on the wasm crate — plain `cargo build --release` never
touches it.

You do not need `embed-web` to run a server. Without it `--server` still
serves the WebSocket backend for native `--connect` clients; pass `--web-root`
to serve a Trunk-built directory, or browse to the HTTP port and get a one-line
placeholder saying the client wasn't built.

### System dependencies

A native build needs a C toolchain and a handful of libraries on top of Rust:

```sh
# Debian / Ubuntu
sudo apt install build-essential pkg-config cmake autoconf automake libtool \
                 libclang-dev libasound2-dev libopus-dev
# Arch
sudo pacman -S base-devel pkgconf cmake autoconf automake libtool clang alsa-lib opus
# macOS
brew install pkg-config cmake autoconf automake libtool opus
```

- **ALSA** (`libasound2-dev` / `alsa-lib`) is not optional on Linux: the audio
  device layer and the MIDI control input both link it. macOS and Windows use
  their own system audio APIs.
- **CMake**, a **C compiler**, **libclang** (for `bindgen`) and **autoconf /
  automake / libtool** are for RADE, whose build fetches and compiles a
  FARGAN-enabled Opus from source. That fetch means the *first* build needs
  network access; later builds reuse it. It is also the slow part of a clean
  build: RADE's model weights are ~110 MB of generated C.
- **libopus** is strictly optional, but installing it avoids a CMake 4 problem —
  see below.

For the **SoapySDR** backend you need its development libraries and the driver
module(s) for your radio (e.g. `soapysdr`, `soapysdr-module-hackrf`,
`soapysdr-module-lms7` on Arch/Debian-style distros). Everything else — including
the RTL-SDR backend — needs no SDR system library at all, so
`cargo build --release --no-default-features` gives a working binary with no
SoapySDR installed.

#### "Compatibility with CMake < 3.5 has been removed" on CMake 4

Two unrelated Opus builds happen during a full build, which makes this error
easy to misattribute:

- **RADE's Opus** — the patched, FARGAN-enabled one that `vendor/rade_c` fetches
  and builds with **autotools**. Every CMake file involved — the vendored ones
  and the wrapper project `crates/sdroxide-rade/build.rs` generates — requires
  3.16 and configures cleanly under CMake 4.
- **The server's Opus** — audio compression for browser and native remote
  clients, via the `opus` → `audiopus_sys` crates. On Unix `audiopus_sys` probes
  `pkg-config` for a system Opus and, if it finds none, compiles its own
  vendored copy **with CMake**. That copy starts with
  `cmake_minimum_required(VERSION 3.1)`.

CMake 4.0 removed support for pre-3.5 minimums, so on a machine with CMake ≥ 4
and no system Opus the build stops with:

```
CMake Error at CMakeLists.txt:1 (cmake_minimum_required):
  Compatibility with CMake < 3.5 has been removed from CMake.
```

The bare `CMakeLists.txt` there is
`~/.cargo/registry/src/*/audiopus_sys-0.2.2/opus/CMakeLists.txt`, not anything
under `vendor/rade_c` — editing the RADE sources or the generated wrapper has no
effect on it, and 0.2.2 is `audiopus_sys`'s newest release, so there is no
version bump to pick up either. Fix it from either end:

```sh
# Install a system Opus and no CMake build happens at all (see the lists above).
sudo apt install libopus-dev pkg-config

# Or configure the vendored copy anyway — this is what the release workflow
# does. CMake 3.x ignores the variable, so it is harmless to leave set.
export CMAKE_POLICY_VERSION_MINIMUM=3.5
```

The two are not quite equivalent: on glibc Linux `audiopus_sys` links a
*system* Opus dynamically, so a binary built with `libopus-dev` present needs
libopus installed wherever it runs, while the vendored route builds it in. Set
`OPUS_STATIC=1` to link the system one statically instead, or `OPUS_LIB_DIR` to
point at a libopus that `pkg-config` cannot see.

### Native binary

```sh
cargo build --release
./target/release/sdroxide --probe        # verify your device is seen
```

### Browser client

The browser client is a separate WebAssembly crate built with
[Trunk](https://github.com/trunk-rs/trunk) 0.21 or newer (CI pins 0.21.14).
Install it with `cargo install --locked trunk`, or drop a prebuilt binary from
its releases page on your `PATH`:

```sh
cd crates/sdroxide-web && trunk build --release
```

Output lands in `crates/sdroxide-web/dist`. Trunk downloads `wasm-bindgen-cli`
and `wasm-opt` itself the first time, so that run needs network access too.

While working on the UI, skip the embed step entirely and point the server at
the directory — a plain `trunk build` (debug) is much faster, and a browser
reload picks up a rebuild:

```sh
cd crates/sdroxide-web && trunk build && cd ../..
./target/release/sdroxide --server --web-root crates/sdroxide-web/dist
```

### Server with the client baked in

Build in this order, then the binary is self-contained and `--server` needs no
`--web-root`:

```sh
(cd crates/sdroxide-web && trunk build --release)   # 1. produces dist/
cargo build --release --features embed-web          # 2. embeds dist/
```

One wrinkle worth knowing: only a **release** build actually bakes the files in.
A debug build with `embed-web` reads them off disk at run time from the path
recorded at compile time, which is why a debug server picks up a rebuilt web
client without recompiling — and why a release binary does not.

### RTL-SDR permissions

The RTL-SDR backend talks to the dongle directly over USB, so the invoking user
needs access to it.

**Linux.** Install the packaged udev rule and replug the dongle:

```sh
sudo cp packaging/linux/60-sdroxide-rtlsdr.rules /usr/lib/udev/rules.d/
sudo udevadm control --reload
```

The `.deb` installs this for you. If your distribution's `rtl-sdr` package is
already installed, its rules cover the same ids and you need not do anything.
The `dvb_usb_rtl28xxu` DVB driver does **not** need blacklisting — sdroxide
detaches it automatically and the kernel rebinds it when the dongle is
unplugged.

**Windows.** The dongle must be bound to **WinUSB**, which you do once with
[Zadig](https://zadig.akeo.ie/). This is the same step SDR#, gqrx and every
libusb-based program require, so if the dongle already works with any of them
there is nothing to do. Note that Zadig replaces the DVB driver, so the stick
stops working as a TV tuner.

**macOS.** Nothing to do.

If a dongle is present but sdroxide cannot open it, `--probe` says so in words
rather than errnos.

### RX-888 permissions

Same situation as the RTL-SDR — direct USB access — with one wrinkle worth
knowing about.

**Linux.** Install the packaged udev rule and replug the receiver:

```sh
sudo cp packaging/linux/60-sdroxide-rx888.rules /usr/lib/udev/rules.d/
sudo udevadm control --reload
```

The `.deb` installs this for you. The rule covers **two** USB ids, and both are
required: `04b4:00f3` is the bare Cypress FX3 bootloader, which is how the
receiver appears on every plug-in, and `04b4:00f1` is the same device once
sdroxide has uploaded firmware into it. A rule covering only the second looks
right and never works, because the upload happens through the first.

**Windows.** Bind the device to **WinUSB** with [Zadig](https://zadig.akeo.ie/),
once for each of the two ids above.

**macOS.** Nothing to do.

**Getting the full sample rate.** The FX3 bootloader always enumerates at USB
2.0, *even on a perfectly good USB 3 cable and port* — only the firmware
sdroxide uploads re-enumerates at SuperSpeed. So a receiver reported as "USB
2.0" before it is programmed is not a problem. If it is still USB 2.0
afterwards, that is a real cable or port problem, and sdroxide clamps the sample
rate and says so on screen rather than silently dropping samples. `--probe`
reports the link speed.

### SDRplay RSP prerequisites

SDR Oxide does not interface with the USB device itself. It talks to the [SDRplay API](https://www.sdrplay.com/api/)
(v3.x) — a userland library plus a background service that owns the hardware,
and whose installer sets up its own USB permissions. Install it, make sure the
service is running (Linux: `sudo systemctl enable --now sdrplay`; the Windows
and macOS installers start it themselves), and the RSP appears under Rescan in
**Settings → Radio → SDRplay RSP (USB)**. If it doesn't, `sdroxide --probe`
says which piece is missing — the library, the service, or the device.

## Running

```sh
# Native desktop, tuned to 20 m, FT8:
sdroxide --freq 14074000 --mode ft8

# Server: DSP + hardware here, UI in a browser at http://<host>:4950
# (needs a web client: either an embed-web build, or --web-root as below)
sdroxide --server

# Server serving a Trunk-built client from disk instead of an embedded one:
sdroxide --server --web-root crates/sdroxide-web/dist

# Desktop UI driven by a remote server (no web client involved):
sdroxide --connect 192.168.1.10:4950
```

## Startup parameters

| Flag | Description |
| --- | --- |
| `--device <ARGS>` | SoapySDR device args (e.g. `driver=hackrf`). Default: config, then first device found. |
| `--probe` | List devices and their probed capabilities, then exit. |
| `--console` | Terminal (ASCII) waterfall mode, no GUI. |
| `--siggen` | Use the built-in signal generator instead of hardware. |
| `--file <FILE>` | Play a raw interleaved CF32 IQ file instead of hardware. |
| `--freq <HZ>` | Center frequency in Hz (default: where the last session was left; `14200000` on a first run). |
| `--rate <HZ>` | Sample rate in Hz (default: from config). |
| `--gain <DB>` | Overall RX gain in dB (default: hardware AGC / moderate). |
| `--mode <MODE>` | Initial mode: `USB LSB CW AM SAM NFM WFM DIGU DIGL DSB SPEC FT8 FT4 FT2 PSK RTTY OLIVIA THOR FSQ HELL SSTV RIFP WEFAX RFPAINT RADE`. Default: the mode the last session was left in. |
| `--antenna <NAME>` | RX antenna port, as the device names it (`LNAH`, `TX/RX`; see `--probe`). Default: the port the last session was left on. |
| `--tx-antenna <NAME>` | TX antenna port, likewise (`BAND1`, `BAND2`). |
| `--server` | Run as a server: HTTP web client + WebSocket streaming backend. |
| `--connect <HOST[:PORT]>` | Connect as a native remote client to a running server. |
| `--port <PORT>` | Server port (default: from config, `4950`). |
| `--web-root <DIR>` | Directory with the Trunk-built web client, e.g. `crates/sdroxide-web/dist` (default: embedded assets with `--features embed-web`). |
| `--fft <N>` | Spectrum FFT size (default `4096`). |
| `--tx-tune <SECS>` | Headless TX smoke test: key a tune carrier at minimal drive, then exit. |
| `--ft8-cq <SECS>` | Headless FT8 smoke test: call CQ at minimal power, then exit. |
| `--rade-rx <SECS>` | Headless RADE smoke test: receive for SECS seconds and report whether the modem synced. Pair with `--file`. |
| `--oob-tx` | Lift the amateur-band transmit lockout for this run, for licensed out-of-band use. Shows a warning that must be dismissed by hand; never persisted, so it has to be passed again every launch. |
| console extras | `--fps <N>` lines/sec, `--width <CHARS>`, `--db-floor <dBFS>`, `--db-ceil <dBFS>`. |

## Keyboard shortcuts

Active whenever a text field isn't focused. These are the **defaults** — all of
them, plus PTT, band, mode, filter and much else, are rebindable on the
**Controls** tab.

| Key | Action |
| --- | --- |
| `←` / `→` | Tune ∓/± 100 Hz (hold **Shift** for 10 Hz fine steps) |
| `↑` / `↓` | Tune ± 1 kHz |
| `PageUp` / `PageDown` | Tune ± 10 kHz |
| `M` | Toggle mute |
| `N` | Toggle the noise blanker |
| `F` | Fit the panadapter to the full device passband |

## Mouse operation

**Panadapter (spectrum + waterfall)**

| Action | Result |
| --- | --- |
| Left-click | Tune the active VFO to that frequency. In FT8/FT4/FT2, sets the TX audio offset instead. |
| **Shift** + left-click | Place the second receiver: the sub-receiver when SUB is on, VFO B otherwise. Works over a spot box and in FT8/FT4/FT2 too. |
| Drag inside the sub-receiver's passband | Tune the sub-receiver (violet, when SUB is on) instead of panning. |
| Left-drag | Grab and slide the spectrum — pans the view and tunes along with it. |
| Right-drag | Pan the view only (no tuning). |
| Scroll wheel | Zoom in/out around the cursor. |
| Drag a passband edge | Move that filter edge (works on the spectrum and the waterfall). |
| Drag the frequency-scale strip | Resize the spectrum vs. waterfall split. |
| Drag the waterfall / FT8 panel divider | Resize the FT8/FT4/FT2 operating panel. |

**Frequency readout** — scroll the wheel over a digit to step that digit; click
its upper half to increment, lower half to decrement.

**FT8/FT4/FT2 decode list** — click a row to move your TX audio onto that signal
(and preview it on the map); press **REPLY** to start an auto-sequenced QSO.


## Contributing, LLM Usage, Licensing

Both local and hosted LLMs (usually advertised as "Generative AI") were used in 
the development of this software. Contributions written using LLMs are ok 
provided the following rules are observed:

* **Read and review** generated code. You should be able to answer questions 
about your contribution.
* **Document and comment** non-trivial parts of the code.
* **Test** your contribution using real radio equipment. If this is not possible,
consider if this is a useful contribution and disclose the need for testing help
before you start.
* Don't use LLMs for trivial things like changing a constant. This is slow,  wasteful
and runs the risk of unneccessary modifications elsewhere.
* Use modern, sufficiently sized models with sufficient context size. Running 
small or outdated models or limiting them to small contexts results in low 
quality code and damage to existing functionality.
* Usage of locally-hosted LLMs is encouraged, but not required.
* Please keep commits vendor-neutral and don't commit specific files for 
one specific cloud hosted LLM.
* Observe the project license. This is a GPLv3 project. Changing the license 
would violate the terms of several of the used libraries.

One part goes further than GPLv3, and it is worth knowing about before you
deploy rather than after. CW decoding uses the
[DeepCW](https://github.com/e04/deepcw-engine) model, which is **AGPL-3.0-only**
and is linked into the binary rather than read as a data file — so its terms
cover the built program as a whole. The practical difference is AGPL section 13:
**running `sdroxide --server` and letting other people use that instance over a
network counts as conveying it to them, so they have to be offered the
Corresponding Source.** Using sdroxide on your own machine changes nothing. The
model is confined to the `sdroxide-deepcw` crate, and the wasm web client links
none of it.

