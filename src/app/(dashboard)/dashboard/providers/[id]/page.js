import ProviderDetailPage from "./ProviderDetailClient";
import { AI_PROVIDERS } from "@/shared/constants/providers";

export async function generateStaticParams() {
  const ids = Object.keys(AI_PROVIDERS || {});
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
    if (!ids.includes(d)) ids.push(d);
  }
  return ids.map((id) => ({ id }));
}

export default async function Page({ params }) {
  const resolved = await params;
  return <ProviderDetailPage providerId={resolved?.id} />;
}
