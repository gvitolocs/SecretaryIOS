import java.io.*;
import java.net.URLEncoder;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.util.*;

/**
 * Golden-output harness. Every method below is copied VERBATIM (method bodies,
 * branch order, quirks) from the NClientV3 sources under app/src/main/java,
 * with Android-only plumbing (Parcelable, Context, JsonReader, DB) stripped.
 * The JVM executes the original logic; its output is the parity ground truth
 * the Rust port must reproduce bit-for-bit.
 */
public class JavaGolden {

    // ===== from api/enums/TagType.java (values copied verbatim) =====
    enum TagType {
        UNKNOWN(0, "", null),
        PARODY(1, "parody", "parodies"),
        CHARACTER(2, "character", "characters"),
        TAG(3, "tag", "tags"),
        ARTIST(4, "artist", "artists"),
        GROUP(5, "group", "groups"),
        LANGUAGE(6, "language", null),
        CATEGORY(7, "category", null);

        final byte id;
        final String single, plural;

        TagType(int id, String single, String plural) {
            this.id = (byte) id;
            this.single = single;
            this.plural = plural;
        }

        static TagType typeByName(String name) {
            for (TagType t : TagType.values()) if (t.single.equals(name)) return t;
            return UNKNOWN;
        }

        byte getId() { return id; }
        String getSingle() { return single; }
    }

    // ===== from api/enums/* =====
    enum TagStatus { DEFAULT, AVOIDED, ACCEPTED }
    enum TitleType { JAPANESE, PRETTY, ENGLISH }

    enum SortType {
        RECENT_ALL_TIME(null),
        POPULAR_ALL_TIME("popular"),
        POPULAR_WEEKLY("popular-week"),
        POPULAR_DAILY("popular-today"),
        POPULAR_MONTH("popular-month");

        final String urlAddition;
        SortType(String urlAddition) { this.urlAddition = urlAddition; }
        String getUrlAddition() { return urlAddition; }

        // verbatim from SortType.findFromAddition
        static SortType findFromAddition(String addition) {
            if (addition == null) return SortType.RECENT_ALL_TIME;
            for (SortType t : SortType.values()) {
                String url = t.getUrlAddition();
                if (url != null && addition.contains(url)) return t;
            }
            return SortType.RECENT_ALL_TIME;
        }
    }

    // ===== from utility/Utility.java (verbatim) =====
    static final String ORIGINAL_URL = "nhentai.net";

    static String getBaseUrl() { return "https://" + getHost() + "/"; }
    static String getHost() { return "nhentai.net"; }

    private static void parseEscapedCharacter(Reader reader, Writer writer) throws IOException {
        int toCreate, read;
        switch (read = reader.read()) {
            case 'u':
                toCreate = 0;
                for (int i = 0; i < 4; i++) {
                    toCreate *= 16;
                    toCreate += Character.digit(reader.read(), 16);
                }
                writer.write(toCreate);
                break;
            case 'n':
                writer.write('\n');
                break;
            case 't':
                writer.write('\t');
                break;
            default:
                writer.write('\\');
                writer.write(read);
                break;
        }
    }

    static String unescapeUnicodeString(String scriptHtml) {
        if (scriptHtml == null) return "";
        StringReader reader = new StringReader(scriptHtml);
        StringWriter writer = new StringWriter();
        int actualChar;
        try {
            while ((actualChar = reader.read()) != -1) {
                if (actualChar != '\\') writer.write(actualChar);
                else parseEscapedCharacter(reader, writer);
            }
        } catch (IOException ignore) {
            return "";
        }
        return writer.toString();
    }

    // ===== from api/components/Tag.java (verbatim) =====
    static class Tag {
        String name;
        int count, id;
        TagType type;
        TagStatus status = TagStatus.DEFAULT;

        Tag(String text) {
            this.count = Integer.parseInt(text.substring(0, text.indexOf(',')));
            text = text.substring(text.indexOf(',') + 1);
            this.id = Integer.parseInt(text.substring(0, text.indexOf(',')));
            text = text.substring(text.indexOf(',') + 1);
            this.type = TagType.values()[Integer.parseInt(text.substring(0, text.indexOf(',')))];
            this.name = text.substring(text.indexOf(',') + 1);
        }

        Tag(String name, int count, int id, TagType type, TagStatus status) {
            this.name = name;
            this.count = count;
            this.id = id;
            this.type = type;
            this.status = status;
        }

        String toQueryTag(TagStatus status) {
            StringBuilder builder = new StringBuilder();
            if (status == TagStatus.AVOIDED)
                builder.append('-');
            builder
                .append(type.getSingle())
                .append(':')
                .append('"')
                .append(name)
                .append('"');
            return builder.toString();
        }

        String toQueryTag() { return toQueryTag(status); }

        String toScrapedString() {
            return String.format(Locale.US, "%d,%d,%d,%s", count, id, type.getId(), name);
        }
    }

    // ===== from api/components/Ranges.java (verbatim) =====
    static class Ranges {
        static final int UNDEFINED = -1;

        int fromPage = UNDEFINED, toPage = UNDEFINED;
        int fromDate = UNDEFINED, toDate = UNDEFINED;
        TimeUnit fromDateUnit = null, toDateUnit = null;

        enum TimeUnit {
            HOUR('h'), DAY('d'), WEEK('w'), MONTH('m'), YEAR('y');
            final char val;
            TimeUnit(char val) { this.val = val; }
        }

        boolean isDefault() {
            return fromDate == UNDEFINED && toDate == UNDEFINED
                && toPage == UNDEFINED && fromPage == UNDEFINED;
        }

        String toQuery() {
            StringBuilder builder = new StringBuilder();
            if (fromPage != UNDEFINED && toPage != UNDEFINED && fromPage == toPage) {
                builder.append("pages:").append(fromPage).append(' ');
            } else {
                if (fromPage != UNDEFINED) builder.append("pages:>=").append(fromPage).append(' ');
                if (toPage != UNDEFINED) builder.append("pages:<=").append(toPage).append(' ');
            }

            if (fromDate != UNDEFINED && toDate != UNDEFINED && fromDate == toDate) {
                builder.append("uploaded:").append(fromDate).append(fromDateUnit.val);
            } else {
                if (fromDate != UNDEFINED)
                    builder.append("uploaded:>=").append(fromDate).append(fromDateUnit.val).append(' ');
                if (toDate != UNDEFINED)
                    builder.append("uploaded:<=").append(toDate).append(toDateUnit.val);
            }
            return builder.toString().trim();
        }
    }

    // ===== from api/local/LocalSortType.java (verbatim) =====
    enum LocalSortKind { TITLE, DATE, PAGE_COUNT, ARTIST, GROUP, RANDOM }

    static final byte MASK_DESCENDING = (byte) (1 << 7);
    static final byte MASK_TYPE = (byte) (MASK_DESCENDING - 1);

    static LocalSortKind decodeType(int hash) {
        return LocalSortKind.values()[(hash & MASK_TYPE) % LocalSortKind.values().length];
    }

    static boolean decodeDescending(int hash) {
        return (hash & MASK_DESCENDING) != 0;
    }

    static int encodeHash(LocalSortKind type, boolean descending) {
        int hash = type.ordinal();
        if (descending) hash |= MASK_DESCENDING;
        return hash;
    }

    // ===== from api/components/Gallery.java (verbatim static methods) =====
    static String getPathTitle(String title, String defaultValue) {
        if (title == null) return defaultValue;
        String pathTitle = title.replace('/', ' ').replaceAll("[/|\\\\*\"'?:<>]", " ");
        while (pathTitle.contains("  "))
            pathTitle = pathTitle.replace("  ", " ");
        return pathTitle.trim();
    }

    static String getPathTitle(String title) { return getPathTitle(title, ""); }

    // ===== from api/enums/SpecialTagIds.java =====
    static final short LANGUAGE_JAPANESE = 6346;
    static final short LANGUAGE_ENGLISH = 12227;
    static final short LANGUAGE_CHINESE = 29963;

    // loadLanguage logic from Gallery.loadLanguage, over an ordered tag list
    static String loadLanguage(List<Tag> languageTags) {
        for (Tag tag : languageTags) {
            if (tag.type != TagType.LANGUAGE) continue;
            switch (tag.id) {
                case LANGUAGE_JAPANESE: return "JAPANESE";
                case LANGUAGE_ENGLISH: return "ENGLISH";
                case LANGUAGE_CHINESE: return "CHINESE";
            }
        }
        return "UNKNOWN";
    }

    // ===== from api/InspectorV3.java =====
    enum ApiRequestType { BYALL, BYTAG, BYSEARCH, BYSINGLE, RELATED, FAVORITE, RANDOM, RANDOM_FAVORITE }

    // getTag() verbatim over an ordered tag list (first non-language else last)
    static String inspectorGetTag(List<Tag> tags) {
        if (tags == null) return null;
        Tag t = null;
        for (Tag tt : tags) {
            if (tt.type != TagType.LANGUAGE) return tt.name;
            t = tt;
        }
        return t == null ? null : t.name;
    }

    /**
     * searchInspector's decision flow verbatim, emulating the inspector's mutable
     * fields (requestType, query) the same way the original Java does.
     */
    static String[] decideRequestType(String query, List<Tag> tags, boolean rangesDefault, SortType sortType) {
        // returns {requestType, effectiveQuery}
        String q = query;
        ApiRequestType type;
        if (query.isEmpty() && rangesDefault) {
            switch (tags.size()) {
                case 0:
                    type = ApiRequestType.BYALL;
                    if (sortType != SortType.RECENT_ALL_TIME) { // tryByAllPopular verbatim
                        type = ApiRequestType.BYSEARCH;
                        q = "-nclientv3";
                    }
                    break;
                case 1:
                    // BYTAG; else by search for the negative tag
                    if (tags.get(0).status != TagStatus.AVOIDED)
                        type = ApiRequestType.BYTAG;
                    else
                        type = ApiRequestType.BYSEARCH;
                    break;
                default:
                    type = ApiRequestType.BYSEARCH;
                    break;
            }
        } else type = ApiRequestType.BYSEARCH;
        return new String[]{type.name(), q};
    }

    // createUrl verbatim (default charset assumed UTF-8 = TIRAMISU branch)
    static String createUrl(ApiRequestType requestType, String rawQuery, int page, int id,
                            List<Tag> tags, Ranges ranges, SortType sortType) {
        String query;
        try {
            query = rawQuery == null ? null : URLEncoder.encode(rawQuery, Charset.defaultCharset().name());
        } catch (UnsupportedEncodingException ignore) {
            query = rawQuery;
        }
        StringBuilder builder = new StringBuilder(getBaseUrl()).append("api/v2/");
        if (requestType == ApiRequestType.BYALL) {
            builder.append("galleries?page=").append(page);
        } else if (requestType == ApiRequestType.RANDOM) {
            builder.append("galleries/random");
        } else if (requestType == ApiRequestType.RANDOM_FAVORITE) {
            builder.append("favorites/random");
        } else if (requestType == ApiRequestType.BYSINGLE) {
            builder.append("galleries/").append(id).append("?include=related,favorite");
        } else if (requestType == ApiRequestType.FAVORITE) {
            builder.append("favorites?page=").append(page);
            if (query != null && !query.isEmpty())
                builder.append("&q=").append(query);
        } else if (requestType == ApiRequestType.BYSEARCH || requestType == ApiRequestType.BYTAG) {
            builder.append("search?query=").append(query);
            for (Tag tt : tags) {
                if (builder.toString().contains(tt.toQueryTag(TagStatus.ACCEPTED))) continue;
                builder.append('+');
                builder.append(URLEncoder.encode(tt.toQueryTag(), Charset.defaultCharset()));
            }
            if (ranges != null)
                builder.append('+').append(ranges.toQuery());
            builder.append("&page=").append(page);
            if (sortType != null && sortType.getUrlAddition() != null) {
                builder.append("&sort=").append(sortType.getUrlAddition());
            }
        }
        return builder.toString().replace(' ', '+');
    }

    // getBookmarkURL verbatim
    static String getBookmarkURL(String url, int page) {
        if (page < 2) return url;
        else return url.substring(0, url.lastIndexOf('=') + 1);
    }

    // getSearchTitle verbatim
    static String getSearchTitle(String query, String url) {
        if (!query.isEmpty()) return query;
        return url.replace(getBaseUrl() + "api/v2/search?query=", "").replace('+', ' ');
    }

    // =================== golden generation ===================
    public static void main(String[] args) throws Exception {
        Map<String, Object> goldens = new LinkedHashMap<>();

        // --- unescapeUnicodeString ---
        // NOTE: build backslash sequences at runtime — javac's unicode-escape
        // preprocessing would otherwise mangle "\\uXXXX" source literals.
        String bs = String.valueOf((char) 0x5C); // a single backslash
        String[] unescapeInputs = {
            "hello" + bs + "u0020world",
            "plain text",
            "back" + bs + "nslash-newline" + bs + "ttab",
            "escaped" + bs + "'quote",
            bs + "u4e2d" + bs + "u6587" + bs + "u30c6" + bs + "u30b9" + bs + "u30c8",
            "trailing" + bs,
            bs + "u12short",
            bs + "uZZZZinvalid",
            "mixed " + bs + "u0041 " + bs + "q end",
            "",
            "double" + bs + bs + "backslash",
            bs + "u10FFFF-overlong",
            "backslash-n-at-" + bs + "nEOF"
        };
        Map<String, String> unescape = new LinkedHashMap<>();
        for (String s : unescapeInputs) unescape.put(s, unescapeUnicodeString(s));
        goldens.put("unescape", unescape);
        goldens.put("unescape_null", unescapeUnicodeString(null));

        // --- toQueryTag ---
        Tag t1 = new Tag("big breasts", 100, 42, TagType.TAG, TagStatus.DEFAULT);
        Tag t2 = new Tag("english", 999, 12227, TagType.LANGUAGE, TagStatus.AVOIDED);
        Tag t3 = new Tag("nakadashi", 555, 777, TagType.TAG, TagStatus.ACCEPTED);
        Tag t4 = new Tag("artist:weird \"name\"", 3, 5, TagType.ARTIST, TagStatus.DEFAULT);
        Map<String, String> queryTags = new LinkedHashMap<>();
        for (Tag t : new Tag[]{t1, t2, t3, t4}) {
            queryTags.put(t.name + "|" + t.status, t.toQueryTag());
            queryTags.put(t.name + "|ACCEPTED", t.toQueryTag(TagStatus.ACCEPTED));
        }
        goldens.put("to_query_tag", queryTags);

        // --- toScrapedString + CSV ctor roundtrip ---
        Map<String, String> scraped = new LinkedHashMap<>();
        for (Tag t : new Tag[]{t1, t2, t3, t4}) scraped.put(t.name, t.toScrapedString());
        goldens.put("to_scraped_string", scraped);
        Map<String, String> csvRoundtrip = new LinkedHashMap<>();
        for (Tag t : new Tag[]{t1, t2, t3, t4}) {
            Tag reparsed = new Tag(t.toScrapedString());
            csvRoundtrip.put(t.name, reparsed.name + "/" + reparsed.count + "/" + reparsed.id + "/" + reparsed.type);
        }
        goldens.put("csv_roundtrip", csvRoundtrip);

        // --- Ranges.toQuery ---
        Map<String, String> ranges = new LinkedHashMap<>();
        Ranges r1 = new Ranges();
        ranges.put("default", r1.toQuery() + "|isDefault=" + r1.isDefault());
        Ranges r2 = new Ranges();
        r2.fromPage = 10; r2.toPage = 10;
        ranges.put("pages_eq", r2.toQuery() + "|isDefault=" + r2.isDefault());
        Ranges r3 = new Ranges();
        r3.fromPage = 5; r3.toPage = 40;
        ranges.put("pages_range", r3.toQuery());
        Ranges r4 = new Ranges();
        r4.fromDate = 3; r4.fromDateUnit = Ranges.TimeUnit.DAY;
        ranges.put("uploaded_from", r4.toQuery());
        Ranges r5 = new Ranges();
        r5.toDate = 2; r5.toDateUnit = Ranges.TimeUnit.WEEK;
        ranges.put("uploaded_to", r5.toQuery());
        Ranges r6 = new Ranges();
        r6.fromDate = 7; r6.toDate = 7; r6.fromDateUnit = Ranges.TimeUnit.MONTH; r6.toDateUnit = Ranges.TimeUnit.MONTH;
        ranges.put("uploaded_eq", r6.toQuery());
        Ranges r7 = new Ranges();
        r7.fromPage = 1; r7.toPage = 200; r7.fromDate = 1; r7.fromDateUnit = Ranges.TimeUnit.YEAR;
        ranges.put("mixed", r7.toQuery());
        Ranges r8 = new Ranges();
        r8.fromPage = 2; r8.toPage = 4; r8.toDate = 1; r8.toDateUnit = Ranges.TimeUnit.HOUR;
        ranges.put("mixed2", r8.toQuery());
        goldens.put("ranges", ranges);

        // --- SortType.findFromAddition ---
        Map<String, String> sortFind = new LinkedHashMap<>();
        for (String s : new String[]{null, "popular", "popular-week", "xpopular-todayx", "popular-month", "recent", "popular-we", ""}) {
            sortFind.put(String.valueOf(s), SortType.findFromAddition(s).name());
        }
        goldens.put("sort_find", sortFind);

        // --- LocalSortType roundtrip ---
        Map<String, String> localSort = new LinkedHashMap<>();
        for (LocalSortKind k : LocalSortKind.values()) {
            for (boolean desc : new boolean[]{false, true}) {
                int hash = encodeHash(k, desc);
                localSort.put(k + "/" + desc,
                    "hash=" + hash + ";type=" + decodeType(hash) + ";desc=" + decodeDescending(hash));
            }
        }
        // decode of a raw hash larger than enum length (Java modulo behavior)
        localSort.put("raw130", "type=" + decodeType(130) + ";desc=" + decodeDescending(130));
        localSort.put("raw-128", "type=" + decodeType(-128) + ";desc=" + decodeDescending(-128));
        goldens.put("local_sort", localSort);

        // --- getPathTitle ---
        Map<String, String> pathTitles = new LinkedHashMap<>();
        String[] titles = {
            "[Artist] My (Great) Title / Vol. 2?",
            "slashes/and|pipes*star\"quote'apos:colon<lt>gt",
            "  double   spaces  ",
            "tab\tchar",
            "",
        };
        for (String s : titles) pathTitles.put(s, getPathTitle(s));
        pathTitles.put("null-default-", getPathTitle(null, "fallback"));
        goldens.put("path_title", pathTitles);

        // --- loadLanguage ---
        Map<String, String> langs = new LinkedHashMap<>();
        langs.put("english", loadLanguage(List.of(new Tag("english", 0, LANGUAGE_ENGLISH, TagType.LANGUAGE, TagStatus.DEFAULT))));
        langs.put("japanese", loadLanguage(List.of(new Tag("japanese", 0, LANGUAGE_JAPANESE, TagType.LANGUAGE, TagStatus.DEFAULT))));
        langs.put("chinese", loadLanguage(List.of(new Tag("chinese", 0, LANGUAGE_CHINESE, TagType.LANGUAGE, TagStatus.DEFAULT))));
        langs.put("other", loadLanguage(List.of(new Tag("korean", 0, 1, TagType.LANGUAGE, TagStatus.DEFAULT))));
        langs.put("mixed", loadLanguage(List.of(
            new Tag("artist", 0, 9, TagType.ARTIST, TagStatus.DEFAULT),
            new Tag("chinese", 0, LANGUAGE_CHINESE, TagType.LANGUAGE, TagStatus.DEFAULT),
            new Tag("english", 0, LANGUAGE_ENGLISH, TagType.LANGUAGE, TagStatus.DEFAULT))));
        goldens.put("load_language", langs);

        // --- InspectorV3 createUrl matrix ---
        List<Tag> searchTags = List.of(
            t3,                                             // accepted tag
            new Tag("vanilla", 200, 300, TagType.TAG, TagStatus.DEFAULT),
            t2                                              // avoided language tag
        );
        Map<String, Object> urls = new LinkedHashMap<>();
        urls.put("byall", createUrl(ApiRequestType.BYALL, null, 0, 0, List.of(), null, SortType.RECENT_ALL_TIME));
        urls.put("byall_p3", createUrl(ApiRequestType.BYALL, null, 3, 0, List.of(), null, SortType.RECENT_ALL_TIME));
        urls.put("random", createUrl(ApiRequestType.RANDOM, null, 0, 0, List.of(), null, null));
        urls.put("random_fav", createUrl(ApiRequestType.RANDOM_FAVORITE, null, 0, 0, List.of(), null, null));
        urls.put("bysingle", createUrl(ApiRequestType.BYSINGLE, null, 0, 12345, List.of(), null, null));
        urls.put("favorite", createUrl(ApiRequestType.FAVORITE, null, 1, 0, List.of(), null, null));
        urls.put("favorite_q", createUrl(ApiRequestType.FAVORITE, "big hero", 2, 0, List.of(), null, null));
        urls.put("bytag", createUrl(ApiRequestType.BYTAG, "", 1, 0,
            List.of(new Tag("nakadashi", 1, 2, TagType.TAG, TagStatus.ACCEPTED)), null, SortType.POPULAR_ALL_TIME));
        urls.put("bysearch", createUrl(ApiRequestType.BYSEARCH, "gundam", 2, 0, searchTags, null, SortType.POPULAR_WEEKLY));
        Ranges rr = new Ranges();
        rr.fromPage = 1; rr.toPage = 30; rr.fromDate = 1; rr.fromDateUnit = Ranges.TimeUnit.MONTH;
        urls.put("bysearch_ranges", createUrl(ApiRequestType.BYSEARCH, "", 1, 0, searchTags, rr, SortType.POPULAR_MONTH));
        urls.put("bysearch_unicode", createUrl(ApiRequestType.BYSEARCH, bs + "u4e2d" + bs + "u6587 + title", 1, 0,
            List.of(t4), null, SortType.RECENT_ALL_TIME));
        urls.put("bysearch_dup", createUrl(ApiRequestType.BYSEARCH, "", 1, 0,
            List.of(new Tag("dup", 1, 2, TagType.TAG, TagStatus.ACCEPTED), new Tag("dup", 1, 2, TagType.TAG, TagStatus.DEFAULT)),
            null, null));
        goldens.put("urls", urls);

        // --- bookmark url + search title ---
        String bu = (String) urls.get("bysearch");
        Map<String, String> bm = new LinkedHashMap<>();
        bm.put("page1", getBookmarkURL((String) urls.get("byall_p3"), 0));
        bm.put("page2", getBookmarkURL(bu, 2));
        bm.put("search_title", getSearchTitle("", bu));
        bm.put("search_title_q", getSearchTitle("gundam", bu));
        goldens.put("bookmark", bm);

        // --- java URLEncoder reference outputs (Rust must match exactly) ---
        Map<String, String> enc = new LinkedHashMap<>();
        for (String s : new String[]{"hello world", "a+b", bs + "u4e2d" + bs + "u6587", "100%", "tag/name", "a*b-c_d.e", "quote\"'?"}) {
            enc.put(s, URLEncoder.encode(s, StandardCharsets.UTF_8));
        }
        goldens.put("url_encode", enc);

        // --- searchInspector decision matrix: {query, tags, rangesDefault, sort} -> {type, effectiveQuery} ---
        Map<String, Object> decisions = new LinkedHashMap<>();
        List<Tag> oneAccepted = List.of(new Tag("nakadashi", 1, 2, TagType.TAG, TagStatus.ACCEPTED));
        List<Tag> oneAvoided = List.of(new Tag("english", 1, 12227, TagType.LANGUAGE, TagStatus.AVOIDED));
        List<Tag> twoTags = List.of(oneAccepted.get(0), oneAvoided.get(0));
        decisions.put("empty_no_tags_recent", String.join("|", decideRequestType("", List.of(), true, SortType.RECENT_ALL_TIME)));
        decisions.put("empty_no_tags_popular", String.join("|", decideRequestType("", List.of(), true, SortType.POPULAR_ALL_TIME)));
        decisions.put("empty_one_accepted", String.join("|", decideRequestType("", oneAccepted, true, SortType.RECENT_ALL_TIME)));
        decisions.put("empty_one_avoided", String.join("|", decideRequestType("", oneAvoided, true, SortType.RECENT_ALL_TIME)));
        decisions.put("empty_two_tags", String.join("|", decideRequestType("", twoTags, true, SortType.RECENT_ALL_TIME)));
        decisions.put("query_no_tags", String.join("|", decideRequestType("gundam", List.of(), true, SortType.RECENT_ALL_TIME)));
        decisions.put("empty_ranges_custom", String.join("|", decideRequestType("", oneAccepted, false, SortType.RECENT_ALL_TIME)));
        goldens.put("decisions", decisions);

        // write goldens.json
        StringBuilder sb = new StringBuilder();
        writeValue(sb, goldens, 0);
        Files.writeString(Path.of(args[0]), sb.toString());
        System.out.println("goldens written: " + args[0]);
    }

    // --- tiny deterministic JSON writer (no external deps) ---
    static void writeValue(StringBuilder sb, Object v, int indent) {
        if (v instanceof Map<?, ?> m) {
            sb.append("{");
            boolean first = true;
            for (Map.Entry<?, ?> e : m.entrySet()) {
                if (!first) sb.append(",");
                first = false;
                sb.append("\n").append("  ".repeat(indent + 1));
                writeString(sb, String.valueOf(e.getKey()));
                sb.append(": ");
                writeValue(sb, e.getValue(), indent + 1);
            }
            if (!m.isEmpty()) sb.append("\n").append("  ".repeat(indent));
            sb.append("}");
        } else {
            writeString(sb, String.valueOf(v));
        }
    }

    static void writeString(StringBuilder sb, String s) {
        sb.append('"');
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            if (c == '"') sb.append("\\\"");
            else if (c == '\\') sb.append("\\\\");
            else if (c == '\n') sb.append("\\n");
            else if (c == '\r') sb.append("\\r");
            else if (c == '\t') sb.append("\\t");
            else if (c < 0x20) sb.append(String.format("\\u%04x", (int) c));
            else sb.append(c);
        }
        sb.append('"');
    }
}
