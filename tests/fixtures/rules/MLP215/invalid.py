from manim import RIGHT, Circle, Scene, Square


class DtNoOp(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        square.add_updater(lambda m, dt: None)
        self.wait(3)


class ScopeNoOp(Scene):
    def construct(self):
        background = Square()
        mover = Circle()
        self.add(background, mover)
        background.add_updater(lambda m: None)
        self.play(mover.animate.shift(RIGHT))
