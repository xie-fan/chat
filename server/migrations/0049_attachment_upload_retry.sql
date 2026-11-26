-- 同一批待发文件重试时复用之前的上传。旧客户端的上传键保持为空。
ALTER TABLE attachments ADD COLUMN client_upload_id UUID;
CREATE UNIQUE INDEX idx_attachments_client_upload
    ON attachments (uploader_id, conv_id, client_upload_id) WHERE client_upload_id IS NOT NULL;
CREATE INDEX idx_messages_attachment ON messages (attachment_id) WHERE attachment_id IS NOT NULL;
