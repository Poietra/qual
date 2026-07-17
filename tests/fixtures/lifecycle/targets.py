from manim import MoveToTarget, Restore, Scene, Square


class Targets(Scene):
    def construct(self):
        sq = Square()
        self.add(sq)
        self.play(MoveToTarget(sq))
        sq.generate_target()
        self.play(MoveToTarget(sq))
        sq.save_state()
        self.play(Restore(sq))


class MaybeTarget(Scene):
    def construct(self):
        sq = Square()
        if self.flag:
            sq.generate_target()
        self.play(MoveToTarget(sq))
