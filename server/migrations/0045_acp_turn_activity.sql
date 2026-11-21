-- 0045: 扩展 acp_turns.role 支持 agent_thought（思考过程流）与 tool_call（工具调用生命周期）
DO $$
DECLARE
    cname text;
BEGIN
    SELECT c.conname INTO cname
      FROM pg_constraint c
      JOIN pg_class t ON t.oid = c.conrelid
     WHERE t.relname = 'acp_turns'
       AND c.contype = 'c'
       AND pg_get_constraintdef(c.oid) LIKE '%role%';
    IF cname IS NOT NULL THEN
        EXECUTE format('ALTER TABLE acp_turns DROP CONSTRAINT %I', cname);
    END IF;
END $$;

ALTER TABLE acp_turns
    ADD CONSTRAINT acp_turns_role_check
    CHECK (role IN ('user', 'agent_thought', 'tool_call', 'agent_plan', 'permission', 'terminal_out', 'agent_text'));
