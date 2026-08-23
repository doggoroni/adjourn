use adjourn_cli::fake::{shared_world, FakeNode};
use adjourn_cli::node::NodeClient;
use adjourn_core::delegate_api::{Request, Response};

#[tokio::test]
async fn the_fake_runs_the_real_delegate() {
    let mut node = FakeNode::new(shared_world());
    let resp = node
        .delegate(Request::CreateGameKey {
            label: "alice".into(),
            caller_entropy: Some([9u8; 32]),
        })
        .await
        .expect("delegate call");
    assert!(matches!(resp, Response::GameKey { .. }));
}

#[tokio::test]
async fn two_fakes_share_one_contract_world() {
    let world = shared_world();
    let mut a = FakeNode::new(world.clone());
    let mut b = FakeNode::new(world);

    // Both see the same contract once one of them puts it. Uses a throwaway
    // id and state; the point is the sharing, not the content.
    let id = freenet_stdlib::prelude::ContractInstanceId::new([1u8; 32]);
    a.put_raw(id, b"hello".to_vec());
    assert_eq!(
        b.get(id, false).await.unwrap().as_deref(),
        Some(&b"hello"[..])
    );
}
