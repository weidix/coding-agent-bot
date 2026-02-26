use teloxide::types::{CallbackQuery, User};

pub(super) fn extract_callback_context(query: &CallbackQuery) -> Option<(i64, i64)> {
    let chat_id = query.regular_message()?.chat.id.0;
    let user_id = user_id_from_user(&query.from)?;
    Some((chat_id, user_id))
}

pub(super) fn user_id_from_message(user: Option<&User>) -> Option<i64> {
    user.and_then(user_id_from_user)
}

pub(super) fn username_from_message<'a>(user: Option<&'a User>) -> Option<&'a str> {
    user.and_then(username_from_user)
}

pub(super) fn username_from_user(user: &User) -> Option<&str> {
    user.username.as_deref()
}

fn user_id_from_user(user: &User) -> Option<i64> {
    i64::try_from(user.id.0).ok()
}
