import { Point } from "./point";

export class Box {
    constructor(private p: Point) {}
    area(): number {
        return this.p.x * this.p.y;
    }
}
