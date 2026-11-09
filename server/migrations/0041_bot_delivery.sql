-- Bot API 拉取新消息（GET /v1/bot/updates）的交付游标：这个成员已交付到的 seq，只对 bot 有意义。
-- 和 last_read_seq 分开：bot 自己发言会把 last_read_seq 推到新消息，夹在中间没交付的会被跳过。
-- 拉人入群时写成当时的 last_seq（重新拉回来的 bot 不再收到以前的 @）；已有的 bot 从现在开始，不把历史当新消息。
ALTER TABLE memberships ADD COLUMN delivered_seq bigint NOT NULL DEFAULT 0;

UPDATE memberships m SET delivered_seq = c.last_seq
  FROM conversations c, users u
 WHERE c.id = m.conversation_id AND u.id = m.user_id AND u.bot_owner_id IS NOT NULL;
