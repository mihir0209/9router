import MediaProviderDetailClient from "./MediaProviderDetailClient";
import { MEDIA_PROVIDER_KINDS, AI_PROVIDERS } from "@/shared/constants/providers";

export async function generateStaticParams() {
  const params = [];
  const pIds = Object.keys(AI_PROVIDERS || {});
  const defaults = [
    "openai",
    "anthropic",
    "google",
    "deepseek",
    "groq",
    "qwen",
    "openrouter",
    "custom",
  ];
  for (const d of defaults) {
    if (!pIds.includes(d)) pIds.push(d);
  }
  for (const k of MEDIA_PROVIDER_KINDS || []) {
    for (const id of pIds) {
      params.push({ kind: k.id, id });
    }
  }
  return params;
}

export default async function Page({ params }) {
  const resolved = await params;
  return <MediaProviderDetailClient kind={resolved?.kind} providerId={resolved?.id} />;
}
