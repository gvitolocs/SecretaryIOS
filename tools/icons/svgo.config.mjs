// svgo settings for assets/icons (see docs/design/item-icons.md).
// Keep the viewBox, never swap hex colours for CSS names (the palette check works on hex).
export default {
  multipass: true,
  floatPrecision: 2,
  plugins: [
    {
      name: "preset-default",
      params: {
        overrides: {
          convertColors: { shortname: false, currentColor: false },
        },
      },
    },
    "removeDimensions",
  ],
};
