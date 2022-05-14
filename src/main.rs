use routefinder::Router;
use std::env;
use std::path::PathBuf;
use tide_disco::load_messages;

fn main() {
    let cwd = env::current_dir().unwrap();
    let api_path = [cwd, "api/api.toml".into()].iter().collect::<PathBuf>();
    let api = load_messages(&api_path);
    println!("{}", api["meta"]["FORMAT_VERSION"]);

    let mut router = Router::new();
    router.add("/*", 1).unwrap();
    router.add("/hello", 2).unwrap();
    router.add("/:greeting", 3).unwrap();
    router.add("/hey/:world", 4).unwrap();
    router.add("/hey/earth", 5).unwrap();
    router.add("/:greeting/:world/*", 6).unwrap();

    assert_eq!(*router.best_match("/hey/earth").unwrap(), 5);
    assert_eq!(
        router
            .best_match("/hey/mars")
            .unwrap()
            .captures()
            .get("world"),
        Some("mars")
    );
    assert_eq!(router.matches("/hello").len(), 3);
    assert_eq!(router.matches("/").len(), 1);
}
