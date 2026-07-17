from manim import Scene, Square


def make_square():
    return Square()


class UsesHelper(Scene):
    def construct(self):
        sq = Square()
        self.show(sq)

    def show(self, mob):
        self.add(mob)


class UsesFactory(Scene):
    def construct(self):
        sq = make_square()
        self.add(sq)


class RecScene(Scene):
    def construct(self):
        sq = Square()
        self.ping(sq)

    def ping(self, mob):
        self.add(mob)
        self.pong(mob)

    def pong(self, mob):
        self.remove(mob)
        self.ping(mob)
