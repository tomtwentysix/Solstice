// Kept apart from the SDK headers because Xlib defines macros (None, Bool, Status) that clash.
#include "x11window.h"

#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>

static Display* display;
static Atom wm_delete;

static bool ensure_display() {
    if (!display) {
        XInitThreads();
        display = XOpenDisplay(nullptr);
        if (display)
            wm_delete = XInternAtom(display, "WM_DELETE_WINDOW", False);
    }
    return display != nullptr;
}

unsigned long x11_create_window(int width, int height, const char* title) {
    if (!ensure_display())
        return 0;
    int screen = DefaultScreen(display);
    XSetWindowAttributes attrs{};
    attrs.background_pixel = BlackPixel(display, screen);
    attrs.event_mask = StructureNotifyMask;
    Window w = XCreateWindow(display, RootWindow(display, screen), 0, 0, width, height, 0, CopyFromParent,
                             InputOutput, CopyFromParent, CWBackPixel | CWEventMask, &attrs);
    XStoreName(display, w, title);
    XChangeProperty(display, w, XInternAtom(display, "_NET_WM_NAME", False),
                    XInternAtom(display, "UTF8_STRING", False), 8, PropModeReplace,
                    reinterpret_cast<const unsigned char*>(title), static_cast<int>(__builtin_strlen(title)));
    XClassHint hint{const_cast<char*>("solstice"), const_cast<char*>("uk.co.solarflareit.Solstice")};
    XSetClassHint(display, w, &hint);
    XSetWMProtocols(display, w, &wm_delete, 1);
    x11_set_fixed_size(w, width, height);
    XFlush(display);
    return w;
}

/// A window inside another application's window, for an editor shown in place.
unsigned long x11_create_child(unsigned long parent, int x, int y, int width, int height) {
    if (!ensure_display())
        return 0;
    XSetWindowAttributes attrs{};
    attrs.background_pixel = BlackPixel(display, DefaultScreen(display));
    Window w = XCreateWindow(display, parent, x, y, width, height, 0, CopyFromParent, InputOutput, CopyFromParent, CWBackPixel, &attrs);
    XFlush(display);
    return w;
}

void x11_move(unsigned long w, int x, int y) {
    XMoveWindow(display, w, x, y);
    XFlush(display);
}

/// Resizes without the fixed-size hints a top-level editor window gets.
void x11_resize_child(unsigned long w, int width, int height) {
    XResizeWindow(display, w, width, height);
    XFlush(display);
}

void x11_set_fixed_size(unsigned long w, int width, int height) {
    XSizeHints* hints = XAllocSizeHints();
    hints->flags = PMinSize | PMaxSize;
    hints->min_width = hints->max_width = width;
    hints->min_height = hints->max_height = height;
    XSetWMNormalHints(display, w, hints);
    XFree(hints);
}

void x11_map(unsigned long w) {
    XMapRaised(display, w);
    XFlush(display);
}

/// Brings the window forward, as when its plug-in's Interface button is pressed again.
void x11_raise(unsigned long w) {
    XMapRaised(display, w);
    XEvent ev{};
    ev.xclient.type = ClientMessage;
    ev.xclient.window = w;
    ev.xclient.message_type = XInternAtom(display, "_NET_ACTIVE_WINDOW", False);
    ev.xclient.format = 32;
    ev.xclient.data.l[0] = 2; // from a pager, which window managers honour without focus-stealing checks
    ev.xclient.data.l[1] = CurrentTime;
    XSendEvent(display, DefaultRootWindow(display), False, SubstructureRedirectMask | SubstructureNotifyMask, &ev);
    XFlush(display);
}

void x11_resize(unsigned long w, int width, int height) {
    x11_set_fixed_size(w, width, height);
    XResizeWindow(display, w, width, height);
    XFlush(display);
}

void x11_destroy(unsigned long w) {
    if (display && w) {
        XDestroyWindow(display, w);
        XFlush(display);
    }
}

void x11_pump(void (*closed)(unsigned long)) {
    if (!display)
        return;
    while (XPending(display)) {
        XEvent ev;
        XNextEvent(display, &ev);
        if (ev.type == ClientMessage && static_cast<Atom>(ev.xclient.data.l[0]) == wm_delete)
            closed(ev.xclient.window);
    }
}
