#pragma once

#include <QCoreApplication>
#include <QJSValue>
#include <QList>
#include <QMetaObject>
#include <QObject>
#include <QGuiApplication>
#include <QQmlApplicationEngine>
#include <QRegion>
#include <QWindow>
#include <QQmlEngine>
#include <QScreen>
#include <QString>
#include <QTimer>
#include <QUrl>
#include <QVariant>

namespace blacksheep {

// Tear down the current root objects and load the shell again. The load is
// deferred one event-loop turn so a reload requested from QML itself does not
// delete the object that is calling us.
inline void engine_reload(QQmlApplicationEngine& engine, const QUrl& url) {
  const auto roots = engine.rootObjects();
  for (QObject* root : roots) {
    root->deleteLater();
  }
  QTimer::singleShot(0, &engine, [&engine, url] {
    engine.clearComponentCache();
    engine.clearSingletons();
    engine.load(url);
  });
}

// Call a function declared in QML on `object` by name. Arguments and the
// return value are QVariants, which is how QML declares untyped functions.
inline QVariant invoke_qml_function(QObject& object, const QString& name,
                                    const QList<QVariant>& args, bool& ok) {
  QVariant ret;
  const QByteArray method = name.toUtf8();
  const char* m = method.constData();
  // Q_ARG stores a pointer, so the arguments must outlive the call.
  const QVariant a0 = args.value(0), a1 = args.value(1), a2 = args.value(2), a3 = args.value(3);
  switch (args.size()) {
    case 0:
      ok = QMetaObject::invokeMethod(&object, m, Qt::DirectConnection, Q_RETURN_ARG(QVariant, ret));
      break;
    case 1:
      ok = QMetaObject::invokeMethod(&object, m, Qt::DirectConnection, Q_RETURN_ARG(QVariant, ret),
                                     Q_ARG(QVariant, a0));
      break;
    case 2:
      ok = QMetaObject::invokeMethod(&object, m, Qt::DirectConnection, Q_RETURN_ARG(QVariant, ret),
                                     Q_ARG(QVariant, a0), Q_ARG(QVariant, a1));
      break;
    case 3:
      ok = QMetaObject::invokeMethod(&object, m, Qt::DirectConnection, Q_RETURN_ARG(QVariant, ret),
                                     Q_ARG(QVariant, a0), Q_ARG(QVariant, a1), Q_ARG(QVariant, a2));
      break;
    case 4:
      ok = QMetaObject::invokeMethod(&object, m, Qt::DirectConnection, Q_RETURN_ARG(QVariant, ret),
                                     Q_ARG(QVariant, a0), Q_ARG(QVariant, a1), Q_ARG(QVariant, a2),
                                     Q_ARG(QVariant, a3));
      break;
    default:
      ok = false;
  }
  // JS objects and arrays come back as QJSValue; flatten to plain variants.
  if (ret.userType() == qMetaTypeId<QJSValue>()) {
    ret = ret.value<QJSValue>().toVariant();
  }
  return ret;
}

// QML hands objects and arrays to invokables as QJSValue; turn those into
// plain QVariantMap / QVariantList so Rust can read them.
inline QVariant flatten_js_value(const QVariant& value) {
  if (value.userType() == qMetaTypeId<QJSValue>()) {
    return value.value<QJSValue>().toVariant();
  }
  return value;
}

// Limit where a window accepts input; outside the rectangle clicks fall
// through to whatever is beneath. On Wayland this is the surface's input
// region. A zero-sized rectangle restores the whole window.
inline void set_input_region(QObject& object, int x, int y, int width, int height) {
  QWindow* window = qobject_cast<QWindow*>(&object);
  if (!window) {
    return;
  }
  if (width <= 0 || height <= 0) {
    window->setMask(QRegion());
  } else {
    window->setMask(QRegion(x, y, width, height));
  }
}

// The QScreen for a connector name such as "DP-2", or null. QML cannot get
// a QScreen* from Qt.application.screens, and layer-shell-qt wants one to
// place a surface on a specific output.
inline QScreen* screen_named(const QString& name) {
  const auto screens = QGuiApplication::screens();
  for (QScreen* screen : screens) {
    if (screen->name() == name) {
      // QScreen has no QObject parent, so QML would otherwise assume it owns
      // what an invokable returned and delete it on garbage collection.
      QQmlEngine::setObjectOwnership(screen, QQmlEngine::CppOwnership);
      return screen;
    }
  }
  return nullptr;
}

} // namespace blacksheep
