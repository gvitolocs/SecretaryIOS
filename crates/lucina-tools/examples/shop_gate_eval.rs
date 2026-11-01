//! Live check of the Jev shopping gate (needs JEV_API_KEY): `shop_gate_eval`.
use lucina_grocery::catalog::Catalog;
use lucina_tools::shopping_gate::check;
const CASES: &[(&str, &str)] = &[
    ("Lavatrici", "it"), ("lavatrice", "it"), ("detersivo per i piatti", "it"), ("pastiglie lavastoviglie", "it"),
    ("carta igienica", "it"), ("Nutella", "it"), ("pancetta dolce", "it"), ("pile AA", "it"),
    ("un trapano", "it"), ("Mario", "it"), ("cibo per gatti", "it"), ("prosciutto crudo", "it"),
    ("Washing machines", "en"), ("oat milk", "en"), ("a new sofa", "en"), ("kitchen roll", "en"),
    ("rugbrød", "da"), ("spugne", "it"), ("frigorifero", "it"), ("caffè macinato", "it"),
];
#[tokio::main]
async fn main() {
    let cat = Catalog::from_paths(vec!["/home/nez/Projects/Lucina/data/shopping-catalog.v1.json".into()]);
    let idx = cat.index();
    let jev = lucina_jev::shared();
    for (t, l) in CASES {
        let s = std::time::Instant::now();
        let g = check(jev, &idx, t, l).await;
        println!("{:>5}ms {t:<26} {g:?}", s.elapsed().as_millis());
    }
    println!("{}", jev.stats());
}
