// SPDX-License-Identifier: AGPL-3.0-or-later
// The little Qt that Dawkey needs to show an LV2 plugin's Qt UI (`ui:Qt5UI`,
// `ui:Qt6UI`) in a window of its own, behind a C interface (Qt has none).
// It's built against Qt 5 and Qt 6 (see build.rs), embedded in Dawkey, and
// loaded into a plugin process only when a Qt UI opens; the Qt it uses is
// the one the UI's own library brings. Every call is on the process's main
// thread.

#include <QApplication>
#include <QPointer>
#include <QWidget>

#include <cstdlib>
#include <cstring>

#define EXPORT extern "C" __attribute__((visibility("default")))

namespace {

// QApplication keeps references to these for its lifetime.
int argc = 1;
char name[] = "dawkey";
char *argv[] = {name, nullptr};

void copy(char *to, std::size_t size, const char *message) {
    if (size > 0) {
        std::strncpy(to, message, size - 1);
        to[size - 1] = '\0';
    }
}

} // namespace

// Makes the process's QApplication, unless it has one. 0 on success;
// otherwise why not, in `error`.
EXPORT int dawkey_qt_init(char *error, std::size_t size) {
    if (QCoreApplication::instance() != nullptr) {
        if (qobject_cast<QApplication *>(QCoreApplication::instance()) == nullptr) {
            copy(error, size, "this process has a Qt application without widgets");
            return 1;
        }
        return 0;
    }
    // Qt aborts if it finds no display, so look first.
    if (std::getenv("DISPLAY") == nullptr && std::getenv("WAYLAND_DISPLAY") == nullptr &&
        std::getenv("QT_QPA_PLATFORM") == nullptr) {
        copy(error, size, "there's no display to show on");
        return 1;
    }
    auto *app = new QApplication(argc, argv);
    // Windows close; the application stays (Dawkey says when it's done).
    app->setQuitOnLastWindowClosed(false);
    return 0;
}

// Shows `widget` (a UI's QWidget, with no parent) as a window titled
// `title`. Returns what the other calls take.
EXPORT void *dawkey_qt_show(void *widget, const char *title) {
    auto *w = static_cast<QWidget *>(widget);
    w->setWindowTitle(QString::fromUtf8(title));
    w->show();
    return new QPointer<QWidget>(w);
}

// Runs what Qt has to do now (events, timers, drawing), without waiting.
EXPORT void dawkey_qt_process_events() {
    QCoreApplication::sendPostedEvents();
    QCoreApplication::processEvents(QEventLoop::AllEvents);
}

// Whether the window is still open: its widget exists and the user didn't
// close it.
EXPORT int dawkey_qt_is_open(void *window) {
    auto *w = static_cast<QPointer<QWidget> *>(window);
    return !w->isNull() && (*w)->isVisible();
}

// Hides the window, if its widget is still there, and forgets it. The
// widget is the UI's, which deletes it as it's cleaned up.
EXPORT void dawkey_qt_forget(void *window) {
    auto *w = static_cast<QPointer<QWidget> *>(window);
    if (!w->isNull()) {
        (*w)->hide();
    }
    delete w;
}
