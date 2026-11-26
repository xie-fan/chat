import type { ComponentPropsWithoutRef, ReactNode } from "react";
import type { AttachmentView } from "../../types";
import { attImgSrc } from "../mediaFormat";
import { useAttachmentAccess } from "./attachmentAccess";

type ImageProps = Omit<ComponentPropsWithoutRef<"img">, "alt" | "src"> & {
  attachment: AttachmentView;
  fallback: ReactNode;
  recalledUrl?: string;
};

export default function AttachmentImage({ attachment, fallback, recalledUrl, ...props }: ImageProps) {
  const access = useAttachmentAccess(attachment.recalled ? undefined : attachment.id, !!attachment.has_thumb);
  const src = attImgSrc(attachment, recalledUrl, access?.url);
  return src ? <img {...props} src={src} alt={attachment.file_name} referrerPolicy="no-referrer" /> : <>{fallback}</>;
}
