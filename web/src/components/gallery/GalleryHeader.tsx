import { useLocation } from "@solidjs/router";
import { Show, type JSX } from "solid-js";
import { PageHeader, SectionNav } from "../page";
import { t } from "../../i18n";
import "../../styles/gallery.css";

/** One header across the library and all its shelves and detail routes. */
export function GalleryHeader(props: { count?: number; context?: JSX.Element; action?: JSX.Element }) {
  const location = useLocation();
  const current = () => {
    const path = location.pathname;
    if (/^\/gallery\/albums?(\/|$)/.test(path)) return "albums";
    if (/^\/gallery\/(people|person)(\/|$)/.test(path)) return "people";
    if (/^\/gallery\/pets?(\/|$)/.test(path)) return "pets";
    if (path.startsWith("/gallery/places")) return "places";
    if (path.startsWith("/gallery/shared")) return "shared";
    return "photos";
  };
  return <PageHeader title={t("gallery.title")}
    context={props.context ?? <Show when={props.count !== undefined}>{t("gallery.itemCount", { count: props.count ?? 0 })}</Show>}
    nav={<SectionNav label={t("gallery.sections")} items={[
      { href: "/gallery", label: t("gallery.typePhotos"), current: current() === "photos" },
      { href: "/gallery/albums", label: t("galleryAlbums.title"), current: current() === "albums" },
      { href: "/gallery/people", label: t("galleryPeople.title"), current: current() === "people" },
      { href: "/gallery/pets", label: t("galleryPets.title"), current: current() === "pets" },
      { href: "/gallery/places", label: t("browse.placesTitle"), current: current() === "places" },
      { href: "/gallery/shared", label: t("gallery.sharedSection"), current: current() === "shared" },
    ]} />} action={props.action} />;
}

export function GalleryDetail(props: { href: string; back: string; title: JSX.Element }) {
  return <div><a class="pg-link pg-back" href={props.href}>‹ {props.back}</a><h2 class="g-detail-title">{props.title}</h2></div>;
}
