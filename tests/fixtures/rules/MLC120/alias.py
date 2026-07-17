import manim as mn


class Demo(mn.Scene):
    def construct(self):
        sq = mn.Square()
        self.add(sq)
        self.play(mn.Restore(sq))
