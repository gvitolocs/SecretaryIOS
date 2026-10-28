//! "Transcribe this video … then do a report" with a YouTube link (typed, pasted, or shared
//! from the iPhone; any channel): answer at once and start the `youtube_report` job
//! (crate::video). The job's answer comes later on the same chat, with a notification.

use lucina_store::jobs::Job;
use lucina_store::timeutil::now_iso;

use super::{Finish, Turn};
use crate::video::{self, archive::Archive, report, trigger};

pub(super) fn try_turn(t: &Turn<'_>, cmid: Option<&str>) -> Option<Finish> {
    let st = t.st;
    if t.guarded || !st.cfg.video.enabled {
        return None;
    }
    let request = trigger::owner_request(&t.message);
    let (url, id) = trigger::find(&t.message)?;
    if !trigger::wants_report(&request) {
        return None;
    }
    let redo = trigger::wants_redo(&request);
    if video::is_running(&id) {
        return Some(Finish::reply(
            "I'm already working on that video; the report will appear here when it's ready.",
            video::ACTION,
        ));
    }
    if !redo {
        if let Some(done) = Archive::new(&st.cfg.video.archive_dir)
            .find(&id)
            .filter(|e| e.note_slug.is_some())
        {
            let (reply, _) = report::already_done(&done);
            return Some(Finish::reply(reply, video::ACTION));
        }
    }
    let now = now_iso();
    let job = Job {
        id: uuid::Uuid::new_v4().to_string(),
        action: video::ACTION.into(),
        status: "queued".into(),
        result: None,
        error: None,
        image_url: None,
        session_id: t.session.clone(),
        client_message_id: cmid.map(str::to_owned),
        created_at: now.clone(),
        started_at: None,
        finished_at: None,
        updated_at: now,
        timeline_item_id: None,
        owner: None,
        boot_id: None,
        extra: Default::default(),
    };
    match st.jobs.insert(job) {
        Ok(job) => {
            let mut f = Finish::reply(
                "On it: I'm downloading the video to the archive, then I'll transcribe it \
                 (Whisper and ZCode), go through its frames and write the report. I'll tell you \
                 here when it's ready.",
                video::ACTION,
            );
            f.job = Some(job);
            f.job_plan = Some(video::plan(&url, &id, &request, redo));
            Some(f)
        }
        Err(e) => {
            tracing::error!(error = %e, "youtube report: job insert failed");
            Some(Finish::reply(
                "I could not start the video report.",
                video::ACTION,
            ))
        }
    }
}
