#pragma once

#include <algorithm>

#include <QDebug>
#include <QDirIterator>
#include <QHash>
#include <QIcon>
#include <QImage>
#include <QMutex>
#include <QQmlApplicationEngine>
#include <QQuickImageProvider>
#include <QSet>
#include <QUrlQuery>

#include "rust/cxx.h"

namespace blacksheep {

// image://icon/<name>             -> freedesktop icon theme lookup
// image://icon/<name>?path=<dir>  -> also search an app-provided directory
// image://icon/<name>?color=#rgb  -> recolour, as toolkits do for -symbolic icons
// image://icon/<name>?solid=1     -> scale alpha so the glyph is fully opaque
// image://icon//abs/path.png      -> a file
class ThemeIconProvider : public QQuickImageProvider {
 public:
  ThemeIconProvider() : QQuickImageProvider(QQuickImageProvider::Image) {}

  QImage requestImage(const QString& id, QSize* size, const QSize& requestedSize) override {
    QString name = id;
    QString dir;
    QColor color;
    bool solid = false;
    const int q = id.indexOf(QLatin1Char('?'));
    if (q >= 0) {
      const QUrlQuery query(id.mid(q + 1));
      dir = query.queryItemValue(QStringLiteral("path"));
      color = QColor(query.queryItemValue(QStringLiteral("color")));
      solid = query.queryItemValue(QStringLiteral("solid")) == QLatin1String("1");
      name = id.left(q);
    }
    const int px = requestedSize.width() > 0 ? requestedSize.width() : 48;

    QIcon icon;
    if (!dir.isEmpty()) {
      icon = fromDirectory(dir, name);
    }
    if (icon.isNull() && name.startsWith(QLatin1Char('/'))) {
      icon = QIcon(name);
    }
    if (icon.isNull()) {
      icon = QIcon::fromTheme(name);
    }
    if (icon.isNull()) {
      warnOnce(name, dir);
      if (size) *size = QSize();
      return QImage();
    }
    QImage image = icon.pixmap(QSize(px, px)).toImage().convertToFormat(QImage::Format_ARGB32);
    // Some themes draw a whole glyph at reduced opacity on purpose (breeze's
    // muted speaker is 35%). A caller that wants it solid anyway says so;
    // alpha is scaled until the most opaque pixel is full, so anti-aliased
    // edges keep their falloff.
    int alphaScaleFrom = 255;
    if (solid) {
      int maxAlpha = 0;
      for (int y = 0; y < image.height(); ++y) {
        const QRgb* line = reinterpret_cast<const QRgb*>(image.constScanLine(y));
        for (int x = 0; x < image.width(); ++x) {
          maxAlpha = std::max(maxAlpha, qAlpha(line[x]));
        }
      }
      if (maxAlpha > 0) alphaScaleFrom = maxAlpha;
    }
    if (color.isValid() || alphaScaleFrom < 255) {
      // Tint: keep each pixel's alpha, replace its colour, as GTK does for
      // symbolic icons.
      const QRgb rgb = color.isValid() ? (color.rgb() & 0x00ffffffu) : 0;
      for (int y = 0; y < image.height(); ++y) {
        QRgb* line = reinterpret_cast<QRgb*>(image.scanLine(y));
        for (int x = 0; x < image.width(); ++x) {
          const int alpha = std::min(255, qAlpha(line[x]) * 255 / alphaScaleFrom);
          const QRgb colour = color.isValid() ? rgb : (line[x] & 0x00ffffffu);
          line[x] = (quint32(alpha) << 24) | colour;
        }
      }
    }
    if (size) *size = image.size();
    return image;
  }

 private:
  // A missing icon is requested again on every reload and by every delegate
  // that shows it; one line per name is enough to diagnose it.
  void warnOnce(const QString& name, const QString& dir) {
    QMutexLocker lock(&m_mutex);
    if (m_reported.contains(name)) {
      return;
    }
    m_reported.insert(name);
    lock.unlock();
    if (dir.isEmpty()) {
      qWarning().noquote() << "blacksheep: no icon named" << name << "in theme" << QIcon::themeName()
                           << "or its fallbacks";
    } else {
      qWarning().noquote() << "blacksheep: no icon named" << name << "in" << dir << "or theme"
                           << QIcon::themeName();
    }
  }

  QMutex m_mutex;
  QSet<QString> m_reported;

  static QIcon fromDirectory(const QString& dir, const QString& name) {
    const QStringList patterns{name + ".png", name + ".svg", name + ".xpm"};
    QDirIterator it(dir, patterns, QDir::Files, QDirIterator::Subdirectories);
    if (it.hasNext()) {
      return QIcon(it.next());
    }
    return QIcon();
  }
};

// image://tray/<item id>?v=<n> -> pixels a StatusNotifierItem sent over D-Bus
class TrayIconProvider : public QQuickImageProvider {
 public:
  TrayIconProvider() : QQuickImageProvider(QQuickImageProvider::Image) {}

  QImage requestImage(const QString& id, QSize* size, const QSize& requestedSize) override {
    QString key = id;
    const int q = id.indexOf(QLatin1Char('?'));
    if (q >= 0) key = id.left(q);

    QImage image;
    {
      QMutexLocker lock(&m_mutex);
      image = m_images.value(key);
    }
    if (image.isNull()) {
      if (size) *size = QSize();
      return image;
    }
    if (requestedSize.width() > 0 && requestedSize.height() > 0) {
      image = image.scaled(requestedSize, Qt::KeepAspectRatio, Qt::SmoothTransformation);
    }
    if (size) *size = image.size();
    return image;
  }

  void set(const QString& key, QImage image) {
    QMutexLocker lock(&m_mutex);
    m_images.insert(key, std::move(image));
  }

  void remove(const QString& key) {
    QMutexLocker lock(&m_mutex);
    m_images.remove(key);
  }

 private:
  QMutex m_mutex;
  QHash<QString, QImage> m_images;
};

// Owned by the engine once installed; the engine lives for the whole process.
inline TrayIconProvider* g_tray_icons = nullptr;

inline void install_icon_providers(QQmlApplicationEngine& engine, const QString& theme) {
  // Without a Qt platform theme (plain Wayland session) Qt has no icon theme
  // name, so take the one from the desktop's GTK settings.
  if (!theme.isEmpty() && (QIcon::themeName().isEmpty() || QIcon::themeName() == QLatin1String("hicolor"))) {
    QIcon::setThemeName(theme);
  }
  engine.addImageProvider(QStringLiteral("icon"), new ThemeIconProvider);
  g_tray_icons = new TrayIconProvider;
  engine.addImageProvider(QStringLiteral("tray"), g_tray_icons);
}

// `bgra` holds width*height*4 bytes in QImage::Format_ARGB32 memory order.
inline void set_tray_icon(const QString& id, int32_t width, int32_t height, rust::Slice<const uint8_t> bgra) {
  if (!g_tray_icons || width <= 0 || height <= 0 || bgra.size() != size_t(width) * size_t(height) * 4) {
    return;
  }
  const QImage view(bgra.data(), width, height, width * 4, QImage::Format_ARGB32);
  g_tray_icons->set(id, view.copy());
}

inline void remove_tray_icon(const QString& id) {
  if (g_tray_icons) g_tray_icons->remove(id);
}

}  // namespace blacksheep
