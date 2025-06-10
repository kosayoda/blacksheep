#![allow(unused_attributes)]

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;

        include!("cxx-qt-lib/qdatetime.h");
        type QDateTime = cxx_qt_lib::QDateTime;

        include!("cxx-qt-lib/qtime.h");
        type QTime = cxx_qt_lib::QTime;
    }

    #[qenum(Clock)]
    enum Precision {
        Second,
        Minute,
        Hour,
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(QDateTime, date, READ, NOTIFY = date_changed)]
        #[qproperty(QTime, time, READ, NOTIFY = date_changed)]
        #[qproperty(u32, hour, READ = hour)]
        #[qproperty(u32, minute, READ = minute)]
        #[qproperty(u32, second, READ = second)]
        #[qproperty(Precision, precision, READ, WRITE, NOTIFY = precision_changed)]
        #[namespace = "blacksheep"]
        type Clock = super::ClockRust;

        #[qsignal]
        #[cxx_name = "dateChanged"]
        fn date_changed(self: Pin<&mut Clock>);

        #[qsignal]
        #[cxx_name = "precisionChanged"]
        fn precision_changed(self: Pin<&mut Clock>);

        #[cxx_name = "hour"]
        fn hour(self: &Clock) -> u32;

        #[cxx_name = "minute"]
        fn minute(self: &Clock) -> u32;

        #[cxx_name = "second"]
        fn second(self: &Clock) -> u32;
    }

    impl cxx_qt::Initialize for Clock {}
    impl cxx_qt::Threading for Clock {}
}

use std::sync::Arc;

use cxx_qt::CxxQtType;
use cxx_qt::Threading;
use cxx_qt_lib::{QDateTime, QTime};
use futures_lite::future::FutureExt as _;
use parking_lot::Mutex;
use tokio::sync::Notify;

pub struct ClockRust {
    date: QDateTime,
    time: QTime,
    precision: qobject::Precision,
}

impl Default for ClockRust {
    fn default() -> Self {
        let date = QDateTime::current_date_time();
        Self {
            time: date.time(),
            date,
            precision: qobject::Precision::Minute,
        }
    }
}

impl ClockRust {
    fn precision_as_unit(&self) -> jiff::Unit {
        match self.precision {
            qobject::Precision::Second => jiff::Unit::Second,
            qobject::Precision::Minute => jiff::Unit::Minute,
            qobject::Precision::Hour => jiff::Unit::Hour,
            _ => unreachable!(),
        }
    }
}

impl cxx_qt::Initialize for qobject::Clock {
    fn initialize(mut self: core::pin::Pin<&mut Self>) {
        let precision = Arc::new((
            Mutex::new(self.as_ref().rust().precision_as_unit()),
            Notify::new(),
        ));

        {
            let precision = Arc::clone(&precision);
            self.as_mut()
                .on_precision_changed(move |obj| {
                    let unit = obj.as_ref().rust().precision_as_unit();
                    // tracing::debug!("Precision changed to: {unit:?}");

                    *precision.0.lock() = unit;
                    precision.1.notify_one();
                })
                .release();
        }

        let qt_thread = self.qt_thread();

        tokio::spawn(async move {
            loop {
                let unit = *(precision.0).lock();
                let round = jiff::ZonedRound::new()
                    .mode(jiff::RoundMode::Ceil)
                    .smallest(unit);

                let now = jiff::Zoned::now();
                let delay = match now.round(round) {
                    Ok(next_tick) => now.duration_until(&next_tick),
                    Err(err) => {
                        tracing::error!("clock: cannot compute next tick: {err}");
                        jiff::SignedDuration::from_secs(1)
                    }
                };
                let delay = if delay.is_positive() {
                    delay.unsigned_abs()
                } else {
                    std::time::Duration::from_secs(1)
                };

                let _ = qt_thread.queue(|mut obj| {
                    let date = QDateTime::current_date_time();
                    let time = date.time();

                    {
                        let mut rust = obj.as_mut().rust_mut();
                        rust.time = time;
                        rust.date = date;
                    }
                    obj.as_mut().date_changed();
                });

                tokio::time::sleep(delay).race(precision.1.notified()).await;
            }
        });
    }
}

impl qobject::Clock {
    fn hour(&self) -> u32 {
        self.rust().time.hour() as u32
    }

    fn minute(&self) -> u32 {
        self.rust().time.minute() as u32
    }

    fn second(&self) -> u32 {
        self.rust().time.second() as u32
    }
}
