import ComboDetailClient from "./ComboDetailClient";

export async function generateStaticParams() {
  return [
    { id: "image-combo" },
    { id: "tts-combo" },
    { id: "default" },
    { id: "custom" },
  ];
}

export default async function Page({ params }) {
  const resolved = await params;
  return <ComboDetailClient comboId={resolved?.id} />;
}
