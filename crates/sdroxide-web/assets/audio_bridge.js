// Audio bridge for the sdroxide web client.
//
// Downlink: wasm pushes mono 48 kHz PCM (Float32Array) -> playback worklet.
// Uplink: capture worklet posts mic blocks here; wasm polls pullMic().
//
// The AudioContext can only start after a user gesture, so everything is
// initialized lazily on the first click/keydown.

(function () {
    let ctx = null;
    let player = null;
    let micChunks = [];
    let micStarted = false;
    let initStarted = false;

    // AudioWorklet and getUserMedia are both secure-context-only, and the whole
    // audio path runs through them. Over plain http a browser only treats
    // localhost as secure, so a page opened at http://<lan-ip>:4950 gets no
    // receive audio and no microphone at all. That is a browser rule, not
    // something the server can opt out of, so say so instead of failing mute.
    const INSECURE = !window.isSecureContext;

    function warnInsecure() {
        // The solar view is a silent viewer; an audio warning there is noise.
        if (location.search.includes("view=solar")) return;
        console.warn(
            "sdroxide audio: this page is not a secure context, so the browser " +
            "withholds AudioWorklet and the microphone. Receive audio and " +
            "microphone transmit are unavailable. Reach the server over HTTPS " +
            "(a reverse proxy or a VPN), or via localhost through an SSH tunnel."
        );
        const show = () => {
            const bar = document.createElement("div");
            bar.style.cssText =
                "position:fixed;left:0;right:0;top:0;z-index:9999;padding:8px 12px;" +
                "font:13px system-ui,sans-serif;background:#4a3000;color:#ffd479;" +
                "border-bottom:1px solid #7a5000;cursor:pointer";
            bar.textContent =
                "No audio: " + location.protocol + "//" + location.host +
                " is not a secure origin, so this browser withholds audio playback " +
                "and microphone access. Use HTTPS or an SSH tunnel to localhost. " +
                "(Click to dismiss.)";
            bar.addEventListener("click", () => bar.remove(), { once: true });
            document.body.appendChild(bar);
        };
        // This script runs from <head>, so <body> may not exist yet.
        if (document.body) show();
        else window.addEventListener("DOMContentLoaded", show, { once: true });
    }

    async function init() {
        if (initStarted) return;
        initStarted = true;
        try {
            ctx = new AudioContext({ sampleRate: 48000 });
            await ctx.audioWorklet.addModule("pcm_worklet.js");
            player = new AudioWorkletNode(ctx, "pcm-player", {
                outputChannelCount: [1],
            });
            player.connect(ctx.destination);
            if (ctx.state === "suspended") {
                await ctx.resume();
            }
            console.log("sdroxide audio: playback ready at", ctx.sampleRate, "Hz");
        } catch (e) {
            console.warn("sdroxide audio init failed:", e);
        }
        startMic();
    }

    async function startMic() {
        if (micStarted || !ctx) return;
        micStarted = true;
        try {
            const stream = await navigator.mediaDevices.getUserMedia({
                audio: { sampleRate: 48000, channelCount: 1 },
            });
            const src = ctx.createMediaStreamSource(stream);
            const capture = new AudioWorkletNode(ctx, "mic-capture");
            capture.port.onmessage = (ev) => {
                micChunks.push(ev.data);
                // Bound: ~1 s of backlog.
                while (micChunks.length > 400) micChunks.shift();
            };
            src.connect(capture);
            console.log("sdroxide audio: mic ready");
        } catch (e) {
            console.warn("sdroxide audio: no microphone:", e);
        }
    }

    if (INSECURE) {
        warnInsecure();
    } else {
        window.addEventListener("click", init, { once: false });
        window.addEventListener("keydown", init, { once: false });
    }

    window.sdroxideAudio = {
        pushPcm: function (pcm) {
            if (player) {
                // Copy: the wasm memory view is invalidated on return.
                player.port.postMessage(new Float32Array(pcm));
            }
        },
        pullMic: function () {
            if (micChunks.length === 0) return new Float32Array(0);
            let total = 0;
            for (const c of micChunks) total += c.length;
            const out = new Float32Array(total);
            let off = 0;
            for (const c of micChunks) {
                out.set(c, off);
                off += c.length;
            }
            micChunks = [];
            return out;
        },
    };
})();
