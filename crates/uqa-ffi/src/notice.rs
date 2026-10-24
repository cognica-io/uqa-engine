//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One-notice ownership transfers over the native emission queue.

use uqa_core::memory::MemoryReservation;
use uqa_sql::{NoticeLevel, SQLNotice};

use crate::{
    abi::{
        UQAStringView, UQA_DONE, UQA_NOTICE_LEVEL_DEBUG, UQA_NOTICE_LEVEL_INFO,
        UQA_NOTICE_LEVEL_LOG, UQA_NOTICE_LEVEL_NOTICE, UQA_NOTICE_LEVEL_WARNING, UQA_OK,
    },
    boundary,
    session::{Session, UQAEngine},
};

pub struct UQANotice {
    native: Option<SQLNotice>,
    _memory: MemoryReservation,
}

impl UQANotice {
    fn prepare(notice: &SQLNotice, session: &Session) -> boundary::Result<Box<Self>> {
        let mut memory = session.memory.reserve(size_of::<Self>())?;
        // The original strings move from the native queue. Admit their owned capacities before
        // taking them; no second queue or diagnostic text copies are needed.
        for field in [
            Some(&notice.sqlstate),
            Some(&notice.message),
            notice.detail.as_ref(),
            notice.hint.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            memory.grow(field.capacity())?;
        }
        boundary::boxed(Self {
            native: None,
            _memory: memory,
        })
    }

    fn native(&self) -> &SQLNotice {
        self.native
            .as_ref()
            .expect("published notice has its native value")
    }
}

ffi_fn!(uqa_notice_create(engine: *mut UQAEngine, out: *mut *mut UQANotice) {
    let out = boundary::new_handle(out)?;
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    let Some((native, mut notice)) = session.native.try_take_sql_notice_with(|native| UQANotice::prepare(native, session))? else {
        return Ok(UQA_DONE);
    };
    notice.native = Some(native);
    *out = Box::into_raw(notice);
    Ok(UQA_OK)
});
ffi_fn!(uqa_notice_destroy(notice: *mut *mut UQANotice) { boundary::release(notice) });
ffi_fn!(uqa_notice_get_level(notice: *mut UQANotice, out: *mut u32) {
    let out = boundary::output(out)?;
    *out = match boundary::input(notice)?.native().level {
        NoticeLevel::Debug => UQA_NOTICE_LEVEL_DEBUG,
        NoticeLevel::Log => UQA_NOTICE_LEVEL_LOG,
        NoticeLevel::Info => UQA_NOTICE_LEVEL_INFO,
        NoticeLevel::Notice => UQA_NOTICE_LEVEL_NOTICE,
        NoticeLevel::Warning => UQA_NOTICE_LEVEL_WARNING,
    };
    Ok(UQA_OK)
});

macro_rules! notice_text {
    ($name:ident, $field:expr) => {
        ffi_fn!($name(notice: *mut UQANotice, out: *mut UQAStringView) {
            let out = boundary::output(out)?;
            let read: fn(&SQLNotice) -> Option<&str> = $field;
            match read(boundary::input(notice)?.native()) {
                Some(text) => { *out = text.into(); Ok(UQA_OK) },
                None => Ok(UQA_DONE),
            }
        });
    };
}
notice_text!(uqa_notice_get_sqlstate, |notice: &SQLNotice| Some(
    notice.sqlstate.as_str()
));
notice_text!(uqa_notice_get_message, |notice: &SQLNotice| Some(
    notice.message.as_str()
));
notice_text!(uqa_notice_get_detail, |notice: &SQLNotice| notice
    .detail
    .as_deref());
notice_text!(uqa_notice_get_hint, |notice: &SQLNotice| notice
    .hint
    .as_deref());
