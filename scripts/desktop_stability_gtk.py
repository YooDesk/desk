import os, json, pathlib, gi, hashlib, base64

if os.getuid() == 0:
    raise SystemExit("Run as the graphical user")
runtime = os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
backend = os.environ.get("GDK_BACKEND", "wayland")
if backend not in ("wayland", "x11"):
    raise SystemExit("Set GDK_BACKEND to wayland or x11")
if not os.environ.get("WAYLAND_DISPLAY" if backend == "wayland" else "DISPLAY"):
    raise SystemExit("Set the graphical session display")
os.environ.update(
    GDK_BACKEND=backend,
    GTK_IM_MODULE="gtk-im-context-simple",
    XDG_RUNTIME_DIR=runtime,
    DBUS_SESSION_BUS_ADDRESS=os.environ.get(
        "DBUS_SESSION_BUS_ADDRESS", f"unix:path={runtime}/bus"
    ),
)
os.environ.pop("XDG_SESSION_ID", None)
gi.require_version("Gtk", "3.0")
from gi.repository import Gtk, Gdk, GdkPixbuf, GLib

root = pathlib.Path(
    os.environ.get(
        "YOODESK_STABILITY_FIXTURE_DIR",
        pathlib.Path(runtime) / f"yoodesk-{backend}-test",
    )
)
root.mkdir(mode=0o700, exist_ok=True)
report = {
    "pid": os.getpid(),
    "nativeWayland": backend == "wayland",
    "backend": backend,
    "entryMatches": False,
    "clicks": 0,
}


def save():
    t = root / "report.tmp"
    t.write_text(json.dumps(report))
    t.replace(root / "report.json")


w = Gtk.Window(title="YooDesk isolated desktop validation")
w.set_default_size(700, 300)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=20)
w.add(box)
entry = Gtk.Entry()
entry.set_placeholder_text("YooDesk test input")
box.pack_start(entry, False, False, 20)
entry.connect(
    "changed",
    lambda e: (report.update(entryMatches=e.get_text() == "yoodeskwayland123"), save()),
)
area = Gtk.Image()
area.set_size_request(640, 320)
box.pack_start(area, False, False, 0)
tick = 0


def draw():
    global tick
    tick += 1
    image = GdkPixbuf.Pixbuf.new(GdkPixbuf.Colorspace.RGB, True, 8, 640, 320)
    image.fill(
        (((tick * 5) % 256) << 24)
        | (((tick * 7) % 256) << 16)
        | (((tick * 11) % 256) << 8)
        | 255
    )
    area.set_from_pixbuf(image)
    return True


def heartbeat():
    report.update(
        animationTicks=tick,
        monotonicMs=GLib.get_monotonic_time() // 1000,
        active=w.is_active(),
    )
    save()
    return True


GLib.timeout_add(1000, heartbeat)
GLib.timeout_add(33, draw)
button = Gtk.Button(label="YooDesk test click")
box.pack_start(button, True, True, 20)
button.connect(
    "clicked", lambda b: (report.update(clicks=report["clicks"] + 1), save())
)
clipboard = Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD)
seen = 0
# Ignore a previous run's command, especially its final quit request.
try:
    seen = json.loads((root / "command.json").read_text())["seq"]
except (FileNotFoundError, json.JSONDecodeError, KeyError):
    pass


def poll():
    global seen
    try:
        c = json.loads((root / "command.json").read_text())
        if c["seq"] == seen:
            return True
        seen = c["seq"]
        op = c["op"]

        def done(ok):
            report.update(seq=seen, ok=ok)
            save()

        if op == "reset-entry":
            entry.set_text("")
            entry.grab_focus()
            w.present()
            done(True)
        elif op == "set-text":
            clipboard.set_text("yoodesk-native-clipboard-中文", -1)
            done(True)
        elif op == "read-text":
            clipboard.request_text(
                lambda cb, text, data: done(text == "yoodesk-browser-clipboard-中文"),
                None,
            )
        elif op == "set-image":
            image = GdkPixbuf.Pixbuf.new(GdkPixbuf.Colorspace.RGB, True, 8, 2, 2)
            image.fill(0x112233FF)
            clipboard.set_image(image)
            done(True)
        elif op == "read-image":
            clipboard.request_image(
                lambda cb, img, data: done(
                    img is not None
                    and img.get_width() == 2
                    and img.get_height() == 2
                    and list(img.get_pixels()[:4]) == [17, 34, 51, 255]
                ),
                None,
            )
        elif op == "verify-text":
            clipboard.request_text(lambda cb, text, data: done(text == c["text"]), None)
        elif op == "verify-image":

            def check(cb, img, data):
                if (
                    img is None
                    or img.get_width() != c["width"]
                    or img.get_height() != c["height"]
                ):
                    done(False)
                    return
                source = img.get_pixels()
                rgba = bytearray()
                channels = img.get_n_channels()
                for y in range(img.get_height()):
                    for x in range(img.get_width()):
                        i = y * img.get_rowstride() + x * channels
                        rgba.extend(source[i : i + 3])
                        rgba.append(source[i + 3] if channels == 4 else 255)
                done(hashlib.sha256(rgba).hexdigest() == c["sha256"])

            clipboard.request_image(check, None)
        elif op == "set-text-value":
            clipboard.set_text(c["text"], -1)
            done(True)
        elif op == "set-image-value":
            image = GdkPixbuf.Pixbuf.new_from_bytes(
                GLib.Bytes.new(base64.b64decode(c["data"])),
                GdkPixbuf.Colorspace.RGB,
                True,
                8,
                c["width"],
                c["height"],
                c["width"] * 4,
            )
            clipboard.set_image(image)
            done(True)
        elif op == "quit":
            Gtk.main_quit()
    except (FileNotFoundError, json.JSONDecodeError):
        pass
    return True


def keys(w, e):
    report["keys"] = report.get("keys", 0) + 1
    report["active"] = w.is_active()
    save()
    return False


w.connect("key-press-event", keys)
w.connect("destroy", lambda w: Gtk.main_quit())
w.show_all()
w.maximize()
entry.grab_focus()
w.present()
save()
GLib.timeout_add(100, poll)
Gtk.main()
