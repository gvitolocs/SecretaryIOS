// Every feature's dictionary. To add one: create dict/<feature>.ts like the
// others (English first, then `it` and `da` with the same keys) and list it here.

import { share } from "./dict/share";
import { chats } from "./dict/chats";
import { chat } from "./dict/chat";
import { common } from "./dict/common";
import { films } from "./dict/films";
import { drive } from "./dict/drive";
import { computer } from "./dict/computer";
import { desk } from "./dict/desk";
import { gym } from "./dict/gym";
import { links } from "./dict/links";
import { maps } from "./dict/maps";
import { login } from "./dict/login";
import { media } from "./dict/media";
import { memos } from "./dict/memos";
import { secrets } from "./dict/secrets";
import { models } from "./dict/models";
import { notify } from "./dict/notify";
import { keep } from "./dict/keep";
import { library } from "./dict/library";
import { notes } from "./dict/notes";
import { plugins } from "./dict/plugins";
import { serverHealth } from "./dict/serverHealth";
import { shell } from "./dict/shell";
import { studio } from "./dict/studio";
import { tasks } from "./dict/tasks";
import { wardrobe } from "./dict/wardrobe";
import { viewer } from "./dict/viewer";
import { gallery } from "./dict/gallery";
import { galleryInfo } from "./dict/galleryInfo";
import { galleryShare } from "./dict/galleryShare";
import { galleryCompare } from "./dict/galleryCompare";
import { weight } from "./dict/weight";

export const dictionaries = { chats, library, common, computer, desk, shell, share, chat, login, weight, gym, wardrobe, notify, links, maps, notes, keep, tasks, studio, models, memos, media, films, drive, plugins, serverHealth, viewer, secrets, gallery, galleryInfo, galleryShare, galleryCompare };

export type Dictionaries = typeof dictionaries;
