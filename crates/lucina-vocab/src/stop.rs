//! Common Italian, English and Danish words that the fuzzy matcher must never
//! rewrite (a best-effort list: the threshold is the main guard, this is the
//! second). Compared after [`crate::phonetic::fold`].
use std::collections::HashSet;
use std::sync::OnceLock;

const IT: &str = "il lo la le gli un uno una di del dello della dei degli delle da dal dallo dalla dai dagli dalle in nel nello \
nella nei negli nelle con su sul sullo sulla sui per tra fra e ed o ma che non mi ti si ci vi ne sono sei siamo siete ho hai ha \
abbiamo avete hanno era ero erano sara saro saranno questo questa quello quella quelli queste questi come dove quando perche \
cosa chi piu molto tutto tutti tutta tutte anche ancora sempre mai gia ora adesso oggi domani ieri stasera stamattina casa \
sto stai sta stiamo stanno tornando torno tornare arrivo arrivi arrivare ricordami ricorda ricordare comprare compra latte \
pane acqua mio mia miei mie tuo tua suo sua nostro nostra loro fare faccio fai fa fanno fatto dire dico detto andare vado \
vai va andiamo vanno vedere vedo vedi vede puoi posso puo possiamo vuoi voglio vuole devo devi deve prima dopo sopra sotto \
dentro fuori vicino lontano bene male meglio peggio grazie prego ciao buongiorno buonasera notte giorno giorni sera mattina \
pomeriggio ore ora minuti minuto settimana mese anno anni lista spesa promemoria progetto stato stata ufficio lavoro strada \
via piazza viale numero primo secondo terzo quando quanto quanti quale quali cerca controlla controllare dimmi dammi dai \
mostra metti togli aggiungi aggiungere ogni altro altra altri altre nuovo nuova vecchio solo senza oppure invece perfetto \
subito presto tardi cosi qui qua lascia lasciare chiama chiamare manda mandare scrivi scrivere leggi leggere apri aprire \
chiudi chiudere accendi accendere spegni spegnere orso orsi aarhus arhus copenhagen kobenhavn odense aalborg roma milano torino napoli firenze bologna venezia genova";

const EN: &str = "the a an and or but if of to in on at by for with from up down out over under into onto about after before \
is are was were be been being am do does did done have has had will would can could should shall may might must i you he she \
it we they me my your his her its our their this that these those what which who whom whose where when why how all any some \
no not yes more most less much many few other another each every both either neither than then so very just only also now \
today tomorrow yesterday tonight morning evening afternoon night day days week month year time hour hours minute minutes \
second here there again still never always often check status project remind reminder remember buy milk list shopping \
please thanks thank hello good great well bad better best new old first last next home house horse horses horn course \
work works working worked office street road call calls calling open close send write read show tell give take make \
let get got go going went come came see saw look find found need want like love know think say said turn stop start \
play pause add remove delete update change set put keep bring ask answer help try use used using place places ores";

const DA: &str = "og i jeg det at en den til er som paa de med han af for ikke der var mig sig men et har om vi min havde ham hun \
nu over da fra du ud sin dem os op man hans hvor eller hvad skal selv her alle vil blev kunne ind naar vaere dog noget ville \
jo deres efter ned skulle denne end dette mit ogsaa under have dig anden hende mine alt meget sit sine vor mod disse hvis din \
nogle hos blive mange ad bliver hendes vaeret thi jer saadan ja nej tak hej godmorgen godnat dag dage uge maaned aar tid \
time timer minut minutter nu idag imorgen igaar aften nat morgen hjem hus vej gade kontor arbejde projekt liste indkoebsliste \
husk huske koebe kob mad vand mad maelk brod kom komme kommer gaa gaar gik se ser saa sagde sige siger tag tage giv give \
faa faar fik kan maa skulle bare lige altid aldrig igen stadig allerede rigtig godt dejlig ny nye gammel foerste sidste \
naeste sammen alene uden mellem omkring tilbage frem hvornaar hvorfor hvordan hvilken hvilke";

fn words() -> &'static HashSet<String> {
    static SET: OnceLock<HashSet<String>> = OnceLock::new();
    SET.get_or_init(|| {
        [IT, EN, DA]
            .iter()
            .flat_map(|l| l.split_whitespace())
            .map(crate::phonetic::fold)
            .collect()
    })
}

/// Is `folded` (already lowercase, ASCII-folded) a common word?
pub(crate) fn is_common(folded: &str) -> bool {
    words().contains(folded)
}
