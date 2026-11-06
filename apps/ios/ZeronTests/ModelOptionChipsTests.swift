import XCTest
@testable import Zeron

final class ModelOptionChipsTests: XCTestCase {
    private let contextWindow = ModelOption(
        id: "contextWindow",
        label: "Context Window",
        choices: [ModelOptionChoice(id: "200k", label: "200K"), ModelOptionChoice(id: "1m", label: "1M")],
        defaultChoice: "200k"
    )
    private let fastMode = ModelOption(
        id: "fastMode",
        label: "Fast Mode",
        choices: [ModelOptionChoice(id: "off", label: "Off"), ModelOptionChoice(id: "on", label: "On")],
        defaultChoice: "off"
    )

    func testChipShowsDefaultThenPick() {
        let unpicked = ModelOptionChips.chips(options: [contextWindow], picks: [:])
        XCTAssertEqual(unpicked.map(\.title), ["200K context"])
        let picked = ModelOptionChips.chips(options: [contextWindow], picks: ["contextWindow": "1m"])
        XCTAssertEqual(picked.map(\.title), ["1M context"])
        XCTAssertEqual(ModelOptionChips.optionId(chip: picked[0].id), "contextWindow")
    }

    func testCursorContextParameterIsAChip() {
        let cursor = ModelOption(id: "context", label: "context", choices: [ModelOptionChoice(id: "256k", label: "256k"), ModelOptionChoice(id: "500k", label: "500k")], defaultChoice: "500k")
        XCTAssertEqual(ModelOptionChips.chips(options: [cursor], picks: [:]).map(\.title), ["500k context"])
    }

    func testOnlyShownOptionsBecomeChips() {
        XCTAssertTrue(ModelOptionChips.chips(options: [fastMode], picks: [:]).isEmpty)
        XCTAssertTrue(ModelOptionChips.chips(options: [], picks: ["contextWindow": "1m"]).isEmpty)
    }

    func testOfferedDropsPicksTheModelLacks() {
        let picks = ["contextWindow": "1m", "fastMode": "on"]
        // A model without a 1M window must not inherit the pick: Claude
        // appends `[1m]` to any model id when it's set.
        XCTAssertEqual(ModelOptionChips.offered(picks, by: [fastMode]), ["fastMode": "on"])
        XCTAssertEqual(ModelOptionChips.offered(picks, by: [contextWindow, fastMode]), picks)
        XCTAssertEqual(ModelOptionChips.offered(["contextWindow": "2m"], by: [contextWindow]), [:])
    }
}
