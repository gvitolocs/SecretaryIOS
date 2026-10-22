// Full-page navigation to another origin (OAuth consent screens). A seam
// for tests, which replace `nav.assign`.

export const nav = {
  assign(url: string): void {
    window.location.assign(url);
  },
};
