import MediaProviderKindClient from "./MediaProviderKindClient";
import { MEDIA_PROVIDER_KINDS } from "@/shared/constants/providers";

export async function generateStaticParams() {
  return (MEDIA_PROVIDER_KINDS || []).map((k) => ({ kind: k.id }));
}

export default async function Page({ params }) {
  const resolved = await params;
  return <MediaProviderKindClient kind={resolved?.kind} />;
}
