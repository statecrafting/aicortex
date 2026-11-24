//! Resolution of mentions to entities, and reversible merges (spec 017 B-2,
//! FR-002).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use aicortex_graph::{
    EntityKind, MERGE_RETENTION_SECONDS, MentionRef, Resolution, Via, get, merge, resolve, unmerge,
};
use rahi_types::UnixSeconds;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr002_two_aliases_resolve_to_one_entity_and_a_similar_name_does_not() {
    let node = common::migrated().await;
    let store = node.handle();
    let (scope, scope_id) = common::scope("alice");

    let first = "Dana Whitfield works on Apollo.";
    let a = common::add_memory(&store, &scope, first, 1_700_000_000).await;
    common::embed(&store, &scope_id, a, first.len(), [1.0, 0.0]).await;
    assert_eq!(common::extract(&store, &scope_id, a).await.completed, 1);

    // The same person under another alias, in a context the model finds
    // close: one entity, with the new surface as an alias.
    let second = "D. Whitfield works on Apollo.";
    let b = common::add_memory(&store, &scope, second, 1_700_000_010).await;
    common::embed(&store, &scope_id, b, second.len(), [0.9999, 0.0141]).await;
    assert_eq!(common::extract(&store, &scope_id, b).await.completed, 1);

    // A different person whose name shares a word, in a context the model
    // finds far away: a new entity.
    let third = "Dana Park works on Zephyr.";
    let c = common::add_memory(&store, &scope, third, 1_700_000_020).await;
    common::embed(&store, &scope_id, c, third.len(), [0.0, 1.0]).await;
    assert_eq!(common::extract(&store, &scope_id, c).await.completed, 1);

    let whitfield = common::entity_named(&store, &scope_id, "Dana Whitfield").await;
    assert_eq!(
        common::entity_named(&store, &scope_id, "D. Whitfield").await,
        whitfield,
        "the second alias resolved to the first entity"
    );
    let park = common::entity_named(&store, &scope_id, "Dana Park").await;
    assert_ne!(park, whitfield, "a different person is a different entity");

    let entity = get(&store, &scope_id, &whitfield).await.unwrap().unwrap();
    assert_eq!(entity.kind, EntityKind::Person);
    assert_eq!(entity.canonical, "Dana Whitfield");
    assert_eq!(entity.aliases, vec!["D. Whitfield".to_owned()]);
    assert_eq!(entity.mention_count, 2);
    assert_eq!(
        common::count(
            &store,
            "SELECT COUNT(*) AS count FROM entity WHERE scope_id = ?1 AND kind = 'person'",
            vec![scope_id.as_str().into()],
        )
        .await,
        2
    );
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_exact_then_normalized_then_nothing_without_a_vector() {
    let node = common::migrated().await;
    let store = node.handle();
    let (scope, scope_id) = common::scope("alice");
    let m = common::add_memory(
        &store,
        &scope,
        "Dana Whitfield works on Apollo.",
        1_700_000_000,
    )
    .await;
    common::extract(&store, &scope_id, m).await;
    let dana = common::entity_named(&store, &scope_id, "Dana Whitfield").await;

    let ask = |surface: &'static str, kind| {
        let store = store.clone();
        let scope_id = scope_id.clone();
        async move {
            resolve(
                &store,
                &scope_id,
                &MentionRef {
                    surface,
                    kind,
                    context: None,
                },
            )
            .await
            .unwrap()
        }
    };
    assert_eq!(
        ask("Dana Whitfield", EntityKind::Person).await,
        Resolution::Existing {
            entity: dana.clone(),
            via: Via::Alias
        }
    );
    assert_eq!(
        ask("DANA  whitfield", EntityKind::Person).await,
        Resolution::Existing {
            entity: dana,
            via: Via::Normalized
        }
    );
    assert_eq!(
        ask("Dana Whitfield", EntityKind::Place).await,
        Resolution::New
    );
    assert_eq!(
        ask("Someone Else", EntityKind::Person).await,
        Resolution::New
    );
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_a_merge_records_both_ids_and_is_reversible_inside_its_window() {
    let node = common::migrated().await;
    let store = node.handle();
    let (scope, scope_id) = common::scope("alice");
    let a = common::add_memory(
        &store,
        &scope,
        "Dana Whitfield works on Apollo.",
        1_700_000_000,
    )
    .await;
    let b = common::add_memory(&store, &scope, "Dee Whit works on Apollo.", 1_700_000_010).await;
    common::extract(&store, &scope_id, a).await;
    common::extract(&store, &scope_id, b).await;
    let dana = common::entity_named(&store, &scope_id, "Dana Whitfield").await;
    let dee = common::entity_named(&store, &scope_id, "Dee Whit").await;
    assert_ne!(dana, dee);

    let now = UnixSeconds::new(1_700_001_000);
    let merge_id = merge(&store, &scope_id, &dana, &dee, now).await.unwrap();
    assert_eq!(
        common::entity_named(&store, &scope_id, "Dee Whit").await,
        dana,
        "the absorbed name now resolves to the survivor"
    );
    let merged = get(&store, &scope_id, &dee).await.unwrap().unwrap();
    assert_eq!(merged.id, dana);
    assert_eq!(merged.mention_count, 2);
    assert!(merged.aliases.contains(&"Dee Whit".to_owned()));
    assert_eq!(
        common::count(
            &store,
            "SELECT COUNT(*) AS count FROM entity WHERE scope_id = ?1",
            vec![scope_id.as_str().into()],
        )
        .await,
        3,
        "nothing was deleted: two people and Apollo"
    );
    assert!(merge(&store, &scope_id, &dana, &dee, now).await.is_err());

    unmerge(&store, &scope_id, &merge_id, now).await.unwrap();
    assert_eq!(
        common::entity_named(&store, &scope_id, "Dee Whit").await,
        dee
    );
    assert!(unmerge(&store, &scope_id, &merge_id, now).await.is_err());

    let remerged = UnixSeconds::new(now.get() + 1);
    let again = merge(&store, &scope_id, &dana, &dee, remerged)
        .await
        .unwrap();
    let late = UnixSeconds::new(remerged.get() + MERGE_RETENTION_SECONDS + 1);
    assert!(
        unmerge(&store, &scope_id, &again, late).await.is_err(),
        "a merge is final once its window has closed"
    );
    node.shutdown().await;
}
