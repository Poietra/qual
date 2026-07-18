from manim import Scene, Square


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        if square.submobjects:
            square.add_updater(lambda m, dt: None)
        self.wait(3)
