use asb_interpreter::tags::var_handler::SoundInfoSnapshot;

#[derive(Default)]
pub(super) struct QuerySnapshots {
    pub sound: SoundInfoSnapshot,
    pub backlog: super::text::BacklogSnapshot,
    pub text_metrics: (f32, f32, f32),
}
