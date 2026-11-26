import { useEffect, useState } from "react";
import { files, session, type AttachmentAccess } from "../../api";

export function useAttachmentAccess(id: string | undefined, thumb = false): AttachmentAccess | undefined {
  const generation = session();
  const key = id ? `${generation}:${id}:${thumb}` : undefined;
  const [ready, setReady] = useState<{ key: string; access: AttachmentAccess }>();

  useEffect(() => {
    if (!id || !key) return;
    let current = true;
    void files.access(id, { thumb }).then((access) => {
      if (current) setReady({ key, access });
    }).catch(() => {});
    return () => { current = false; };
  }, [id, key, thumb]);

  return ready?.key === key ? ready?.access : undefined;
}
