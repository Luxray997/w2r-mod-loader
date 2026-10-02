def base_manifest():
    return {
        "schema": 1,
        "id": "example-plugin",
        "name": "Example Plugin",
        "version": "1.2.0",
        "author": "someone",
        "description": "One sentence.",
        "entry": "example_plugin.dll",
        "abi": 1,
        "game_builds": ["1.0.2.2818"],
        "capabilities": ["game.read", "chat.read"],
        "affects_simulation": False,
        "dependencies": [],
    }
