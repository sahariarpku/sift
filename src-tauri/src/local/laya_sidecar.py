"""Sift helper process for Laya-MLX typed decisions.

Protocol: JSON lines. The first line written is either
{"ready": true, "version": "...", "model": "..."} or {"error": "..."}.
Each request line {"state": "...", "questions": {...}} gets one reply line,
{"answers": {...}} or {"error": "..."}. Runs offline (HF_HUB_OFFLINE=1 is set
by Sift); library output is sent to stderr so stdout carries only replies.
"""

import contextlib
import json
import sys
import warnings

warnings.filterwarnings("ignore")
REPLY = sys.stdout


def send(obj):
    REPLY.write(json.dumps(obj) + "\n")
    REPLY.flush()


def main():
    model = sys.argv[1]
    try:
        with contextlib.redirect_stdout(sys.stderr):
            import laya_mlx

            agent = laya_mlx.load(model)
    except Exception as e:  # noqa: BLE001 - reported to Sift verbatim
        send({"error": f"{type(e).__name__}: {e}"})
        return 1
    send({"ready": True, "version": getattr(laya_mlx, "__version__", "unknown"), "model": model})
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            with contextlib.redirect_stdout(sys.stderr):
                result = agent.predict(req["state"], req["questions"])
            send({"answers": result["answers"]})
        except Exception as e:  # noqa: BLE001
            send({"error": f"{type(e).__name__}: {e}"})
    return 0


sys.exit(main())
